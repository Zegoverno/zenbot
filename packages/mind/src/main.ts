// zen-mind: stateless model worker. Speaks JSON-RPC 2.0 (one JSON object per line) over stdio
// with the kernel (zend). The kernel owns all state and executes every tool call.

import { readFileSync } from "node:fs";
import { createInterface } from "node:readline";
import { Agent } from "@earendil-works/pi-agent-core";
import type { AgentTool } from "@earendil-works/pi-agent-core";
import { clampThinkingLevel, createModels, fauxAssistantMessage, fauxProvider, fauxText, fauxToolCall, getSupportedThinkingLevels } from "@earendil-works/pi-ai";
import { openaiProvider } from "@earendil-works/pi-ai/providers/openai";
import { openrouterProvider } from "@earendil-works/pi-ai/providers/openrouter";
import { Type } from "typebox";
import { FileCredentialStore } from "./credentials.ts";

type Json = any;

const authFile = process.env.ZEN_AUTH_FILE ?? `${process.env.HOME}/.zenbot/auth.json`;
const models = createModels({ credentials: new FileCredentialStore(authFile) });
models.setProvider(openaiProvider());
// System One classifiers (e.g. TypeSafe's Jev) for live session scoring, keyed by OPENROUTER_API_KEY.
models.setProvider(openrouterProvider());

// Scripted test model (ZEN_FAUX=1): runs one bash tool call, then answers. For smoke tests and CI.
const faux = process.env.ZEN_FAUX === "1" ? fauxProvider() : undefined;
if (faux) models.setProvider(faux.provider);
function scriptFaux() {
  faux?.setResponses([
    fauxAssistantMessage([fauxToolCall("bash", { command: "echo zen-ok > zen-smoke.txt && cat zen-smoke.txt" })], { stopReason: "toolUse" }),
    fauxAssistantMessage([fauxText("Smoke test passed: I ran a command and wrote zen-smoke.txt.")]),
  ]);
}

// ---- JSON-RPC plumbing ----

let nextId = 1;
const pending = new Map<number, { resolve: (v: Json) => void; reject: (e: Error) => void }>();

function send(msg: Json) {
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...msg }) + "\n");
}
function notify(method: string, params: Json) {
  send({ method, params });
}
function request(method: string, params: Json): Promise<Json> {
  const id = nextId++;
  send({ id, method, params });
  return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
}
function log(...args: unknown[]) {
  process.stderr.write(`[mind] ${args.map(String).join(" ")}\n`);
}

// ---- Tools: kernel sends JSON Schema; we convert the simple subset we use to TypeBox ----

function toTypeBox(schema: Json): Json {
  switch (schema.type) {
    case "object": {
      const required = new Set<string>(schema.required ?? []);
      const props: Record<string, Json> = {};
      for (const [k, v] of Object.entries<Json>(schema.properties ?? {})) {
        const t = toTypeBox(v);
        props[k] = required.has(k) ? t : Type.Optional(t);
      }
      return Type.Object(props, { description: schema.description });
    }
    case "string":
      return Type.String({ description: schema.description });
    case "integer":
      return Type.Integer({ description: schema.description });
    case "number":
      return Type.Number({ description: schema.description });
    case "boolean":
      return Type.Boolean({ description: schema.description });
    default:
      return Type.Any({ description: schema.description });
  }
}

function kernelTools(sessionId: string, specs: Json[]): AgentTool<any>[] {
  return specs.map((spec) => ({
    name: spec.name,
    label: spec.name,
    description: spec.description,
    parameters: toTypeBox(spec.parameters),
    async execute(toolCallId: string, params: Json) {
      const res = await request("tool.call", { session_id: sessionId, call_id: toolCallId, name: spec.name, args: params });
      if (res.is_error) throw new Error(res.content);
      return { content: [{ type: "text", text: res.content }], details: res.details ?? null };
    },
  }));
}

// Pi's version, reported with each turn so a Pi update shows up in the traces.
const piVersion: string = (() => {
  try {
    const path = new URL("../node_modules/@earendil-works/pi-ai/package.json", import.meta.url);
    return JSON.parse(readFileSync(path, "utf-8")).version;
  } catch {
    return "unknown";
  }
})();

// ---- Thinking level ----

// Pi's agent thinks with level "off" unless told otherwise; reasoning models default to "medium" here.
function defaultEffort(model: Json): string {
  return model.reasoning ? clampThinkingLevel(model, "medium") : "off";
}

function modelInfo(model: Json, id: string, name: string): Json {
  return { id, name, context: model.contextWindow, efforts: getSupportedThinkingLevels(model), default_effort: defaultEffort(model) };
}

// ---- Turns ----

const running = new Map<string, Agent>();
const aborted = new Set<string>();

// Never throws: every failure becomes turn.end with an error, so the kernel frees the session.
// The kernel's history as Pi messages: a user message's turn context becomes a second text block
// (as it was sent), a summary of older turns stays a user message, and kernel-only fields go.
function piMessages(history: Json[]): Json[] {
  return history.map((m: Json) => {
    const { seq, context, summary, ...msg } = m;
    if (msg.role === "user" && context) return { ...msg, content: userContent(msg.content, context) };
    return msg;
  });
}
function userContent(content: Json, context?: string): Json {
  const blocks = typeof content === "string" ? [{ type: "text", text: content }] : content;
  return context ? [...blocks, { type: "text", text: context }] : blocks;
}

async function turnStart(p: Json) {
  const { session_id, model: modelRef, effort, system_prompt, history, prompt, prompt_context, tools } = p;
  try {
    const [provider, id] = String(modelRef).split("/");
    const model = models.getModel(provider as any, id);
    if (!model) throw new Error(`unknown model ${modelRef}`);
    if (provider === faux?.provider.id) scriptFaux();

    const agent = new Agent({
      initialState: {
        systemPrompt: system_prompt,
        model,
        thinkingLevel: effort ?? defaultEffort(model),
        tools: kernelTools(session_id, tools),
        messages: piMessages(history ?? []),
      },
      // Long cache retention where the provider has it; the session id lets providers route a
      // session's requests to the same cache.
      streamFn: (model: Json, context: Json, options: Json) => models.streamSimple(model, context, { ...options, cacheRetention: "long" }),
      sessionId: session_id,
    });
    running.set(session_id, agent);

    let callStarted = 0;
    agent.subscribe((ev: Json) => {
      if (ev.type === "message_start" && ev.message?.role === "assistant") callStarted = Date.now();
      if (ev.type === "message_update") {
        const e = ev.assistantMessageEvent;
        if (e?.type === "text_delta") notify("turn.delta", { session_id, delta: e.delta });
        else if (e?.type === "thinking_delta") notify("turn.thinking", { session_id, delta: e.delta });
      } else if (ev.type === "message_end") {
        const role = ev.message?.role;
        if (role === "assistant") notify("turn.message", { session_id, message: { ...ev.message, durationMs: Date.now() - callStarted } });
        else if (role === "toolResult") notify("turn.message", { session_id, message: ev.message });
      }
    });

    await agent.prompt({ role: "user", content: userContent(prompt, prompt_context ?? undefined), timestamp: Date.now() } as Json);
    notify("turn.usage", { session_id, engine: "pi", engine_version: piVersion, render: "native" });
    const err = aborted.has(session_id) ? "interrupted" : agent.state.errorMessage;
    notify("turn.end", { session_id, error: err ?? null });
  } catch (e) {
    const err = aborted.has(session_id) ? "interrupted" : e instanceof Error ? e.message : String(e);
    notify("turn.end", { session_id, error: err });
  } finally {
    running.delete(session_id);
    aborted.delete(session_id);
  }
}

// ---- System One ----

// Answer typed questions about a state with a classifier model ("<provider>/<model id>").
// Errors are returned in the result (the kernel records them with the score), not thrown.
async function decide(p: Json): Promise<Json> {
  const ref = String(p.model ?? "");
  const slash = ref.indexOf("/");
  const model = slash > 0 ? models.getModelOfType("classifier", ref.slice(0, slash) as any, ref.slice(slash + 1)) : undefined;
  if (!model) return { error: `unknown classifier ${ref}` };
  const result: Json = await models.classify(model, { state: p.state, questions: p.questions });
  return {
    model: result.model,
    provider: result.provider,
    answers: result.answers ?? {},
    usage: result.usage ?? null,
    error: result.stopReason === "stop" ? null : (result.errorMessage ?? result.stopReason),
  };
}

// One completion without tools (summaries). Errors are returned in the result.
async function complete(p: Json): Promise<Json> {
  const [provider, id] = String(p.model ?? "").split("/");
  const model = models.getModel(provider as any, id);
  if (!model) return { error: `unknown model ${p.model}` };
  const msg: Json = await models.completeSimple(model, {
    systemPrompt: p.system,
    messages: [{ role: "user", content: p.prompt, timestamp: Date.now() }],
  } as Json);
  if (msg.stopReason === "error") return { error: msg.errorMessage ?? "completion failed" };
  const text = (msg.content ?? []).filter((c: Json) => c.type === "text").map((c: Json) => c.text).join("");
  const u = msg.usage ?? {};
  return { text, model: msg.model, usage: { input: u.input, output: u.output, cache_read: u.cacheRead, cost_usd: u.cost?.total } };
}

async function handle(method: string, params: Json): Promise<Json> {
  switch (method) {
    case "models.list": {
      const creds = await models.getAuth("openai").catch(() => undefined);
      const list = models.getModels("openai").map((m: Json) => modelInfo(m, `openai/${m.id}`, m.name ?? m.id));
      if (faux) {
        const m: Json = faux.getModel();
        list.push(modelInfo(m, `${m.provider}/${m.id}`, "Test model (scripted)"));
      }
      const openrouter = await models.getAuth("openrouter").catch(() => undefined);
      const classifiers = models
        .getModelsOfType("classifier", "openrouter")
        .map((m: Json) => ({ id: `openrouter/${m.id}`, name: m.name ?? m.id, context: m.contextWindow }));
      return { authenticated: { openai: !!creds, openrouter: !!openrouter }, models: list, classifiers };
    }
    case "s1.decide":
      return decide(params);
    case "complete":
      return complete(params).catch((e) => ({ error: e instanceof Error ? e.message : String(e) }));
    case "turn.start":
      void turnStart(params);
      return { ok: true };
    case "turn.abort":
      if (running.has(params.session_id)) aborted.add(params.session_id);
      running.get(params.session_id)?.abort();
      return { ok: true };
    case "ping":
      return { pong: true };
    default:
      throw new Error(`unknown method ${method}`);
  }
}

const rl = createInterface({ input: process.stdin });
rl.on("line", async (line) => {
  if (!line.trim()) return;
  let msg: Json;
  try {
    msg = JSON.parse(line);
  } catch {
    log("bad json from kernel");
    return;
  }
  if (msg.method) {
    try {
      const result = await handle(msg.method, msg.params ?? {});
      if (msg.id !== undefined) send({ id: msg.id, result });
    } catch (e) {
      if (msg.id !== undefined) send({ id: msg.id, error: { code: -32000, message: e instanceof Error ? e.message : String(e) } });
    }
  } else if (msg.id !== undefined) {
    const p = pending.get(msg.id);
    if (!p) return;
    pending.delete(msg.id);
    if (msg.error) p.reject(new Error(msg.error.message));
    else p.resolve(msg.result);
  }
});
rl.on("close", () => process.exit(0));
log("ready");
