// zen-mind: stateless model worker. Speaks JSON-RPC 2.0 (one JSON object per line) over stdio
// with the kernel (zend). The kernel owns all state and executes every tool call.

import { readFileSync } from "node:fs";
import { createInterface } from "node:readline";
import { Agent } from "@earendil-works/pi-agent-core";
import type { AgentTool } from "@earendil-works/pi-agent-core";
import { clampThinkingLevel, createModels, fauxAssistantMessage, fauxProvider, fauxText, fauxToolCall, getSupportedThinkingLevels } from "@earendil-works/pi-ai";
import { openaiProvider } from "@earendil-works/pi-ai/providers/openai";
import { Type } from "typebox";
import { FileCredentialStore } from "./credentials.ts";

type Json = any;

const authFile = process.env.ZEN_AUTH_FILE ?? `${process.env.HOME}/.zenbot/auth.json`;
const models = createModels({ credentials: new FileCredentialStore(authFile) });
models.setProvider(openaiProvider());

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
async function turnStart(p: Json) {
  const { session_id, model: modelRef, effort, system_prompt, history, prompt, tools } = p;
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
        messages: history,
      },
      streamFn: models.streamSimple.bind(models),
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

    await agent.prompt(prompt);
    notify("turn.usage", { session_id, engine: "pi", engine_version: piVersion });
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

async function handle(method: string, params: Json): Promise<Json> {
  switch (method) {
    case "models.list": {
      const creds = await models.getAuth("openai").catch(() => undefined);
      const list = models.getModels("openai").map((m: Json) => modelInfo(m, `openai/${m.id}`, m.name ?? m.id));
      if (faux) {
        const m: Json = faux.getModel();
        list.push(modelInfo(m, `${m.provider}/${m.id}`, "Test model (scripted)"));
      }
      return { authenticated: { openai: !!creds }, models: list };
    }
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
