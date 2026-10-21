//! zen — command-line interface to a running zenbot kernel (zend).
//! Designed for agents first: every command supports --json and exits non-zero on failure.

mod cli;
mod client;
mod editor;
mod md;
mod print;
mod screen;
mod setup;
mod tui;
mod turn;

use std::io::IsTerminal;

use cli::*;
use client::{describe_update, dim, enc, short, Client, NewSession};
use print::*;
use setup::*;
use turn::{ask, chat};

use anyhow::Result;
use clap::Parser;
use serde_json::{json, Value};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        let message = format!("{e:#}");
        eprintln!("error: {}", for_stderr(&message));
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    if let Some(Cmd::Login { which }) = &cli.cmd {
        return login(which.as_deref());
    }
    let c = Client::new(&cli.url, cli.token)?;
    let json = cli.json;
    let out = |v: &Value| emit(true, v, String::new);
    let Some(cmd) = cli.cmd else {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            // Piped use without a command behaves like `zen ask`.
            return ask(&c, json, None, None, NewSession { model: cli.model, effort: cli.effort }, false).await;
        }
        let start = match (cli.cont, cli.resume) {
            (_, Some(id)) if !id.is_empty() => tui::Start::Resume(Some(id)),
            (_, Some(_)) => tui::Start::Resume(None),
            (true, None) => tui::Start::Continue,
            // A model or thinking level asked for means a new session on it; plain `zen` is the board.
            _ if cli.model.is_some() || cli.effort.is_some() => tui::Start::New,
            _ => tui::Start::Board,
        };
        return tui::run(c, start, NewSession { model: cli.model, effort: cli.effort }, cli.inline).await;
    };
    match cmd {
        Cmd::Ask { prompt, session, model, effort, quiet } => ask(&c, json, prompt, session, NewSession { model, effort }, quiet).await?,
        Cmd::Chat { session, model, effort } => {
            let new = NewSession { model, effort };
            if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                let start = session.map(|s| tui::Start::Resume(Some(s))).unwrap_or(tui::Start::New);
                tui::run(c, start, new, cli.inline).await?
            } else {
                chat(&c, session, new).await?
            }
        }
        Cmd::Login { .. } => unreachable!(),
        Cmd::Queue {decision,kind,id} => {
            if let (Some(decision),Some(kind),Some(id))=(&decision,&kind,id) {
                let v=c.post(&format!("/api/queue/{kind}/{id}"),json!({"decision":decision})).await?;
                emit(json,&v,||v.to_string());
            } else if decision.is_some() || kind.is_some() || id.is_some() {
                anyhow::bail!("usage: zen queue [approve|reject|deny <action|source> <id>]");
            } else if !json && std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                tui::run(c,tui::Start::Queue,NewSession {model:None,effort:None},cli.inline).await?;
            } else {
                let v=c.get("/api/queue").await?;
                emit(json,&v,||serde_json::to_string_pretty(&v).unwrap_or_default());
            }
        }
        Cmd::Sources { cmd } => {
            let value = match cmd {
                SourcesCmd::List => c.get("/api/sources").await?,
                SourcesCmd::Rerate => c.post("/api/sources/rerate", json!({})).await?,
                SourcesCmd::Add { source, risk, note } => c.post("/api/sources", json!({"source":source,"risk":risk,"note":note})).await?,
                SourcesCmd::Rm { source } => c.post("/api/sources", json!({"source":source,"remove":true})).await?,
            };
            emit(json, &value, || serde_json::to_string_pretty(&value).unwrap_or_default());
        }
        Cmd::Models => {
            let m = c.get("/api/models").await?;
            if json {
                out(&m);
            } else {
                for x in m["models"].as_array().into_iter().flatten() {
                    let id = x["id"].as_str().unwrap_or("");
                    let default = if Some(id) == m["default"].as_str() { "  (default)" } else { "" };
                    println!("{}{default}{}", for_stdout(id), dim(&effort_list(x)));
                }
            }
        }
        Cmd::Upgrade { check } => {
            if check {
                let v = c.get("/api/version?refresh=true").await?;
                if json {
                    out(&v);
                } else {
                    println!("{}", for_stdout(&describe_update(&v)));
                    for x in v["commits"].as_array().into_iter().flatten() {
                        println!("  · {}", for_stdout(x.as_str().unwrap_or("")));
                    }
                }
            } else {
                let msg = client::upgrade(&c, |l| eprintln!("{}", dim(&l))).await?;
                emit(json, &json!({ "ok": true, "message": msg }), || msg.clone());
            }
        }
        Cmd::Status => {
            let health = c.health().await?;
            let models = c.get("/api/models").await?;
            let any_signed_in = engine_signed_in(&models);
            let version = c.get("/api/version").await.unwrap_or(Value::Null);
            let git_identity = git_identity();
            let engines = engines_state();
            let disk = root_disk();
            let memory = c.get("/api/memory").await.unwrap_or(Value::Null);
            let jobs = c.get("/api/jobs").await.unwrap_or(Value::Null);
            let s1 = c.get("/api/s1/check").await.unwrap_or(Value::Null);
            let status = json!({
                "s1": s1,
                "memory": memory_summary(&memory),
                "jobs": jobs,
                "engines": engines,
                "disk": disk,
                "git_identity": git_identity,
                "commit": health["commit"],
                "update": version,
                "url": c.url,
                "ok": health["ok"] == true && any_signed_in,
                "workers": health["workers"],
                "ignored_workers": health["ignored_workers"],
                "kernel": true,
                "database": health["db"],
                "worker": health["mind"],
                "signed_in": models["authenticated"],
                "default_model": models["default"],
                "scorer": models["scorer"],
            });
            if json {
                out(&status);
            } else {
                let mark = |b: bool| if b { "ok" } else { "FAIL" };
                println!("kernel    {}  ({})", mark(true), for_stdout(&c.url));
                println!("database  {}", mark(health["db"] == true));
                if let Some(line) = disk_line(&disk) {
                    println!("disk      {}", for_stdout(&line));
                }
                for (name, ok) in health["workers"].as_object().into_iter().flatten() {
                    println!("{:<9} {}", format!("worker:{}", for_stdout(name)), mark(*ok == true));
                }
                // Names in ZEN_WORKERS the kernel doesn't know (a retired worker): not a failure.
                for name in health["ignored_workers"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    println!("{:<9} ignored: not a known worker (remove it from ZEN_WORKERS in ~/.zenbot/env)", format!("worker:{}", for_stdout(name)));
                }
                // OpenRouter serves System One; it's shown on the s1 line below.
                for (name, ok) in models["authenticated"].as_object().into_iter().flatten().filter(|(n, _)| *n != "openrouter") {
                    println!("{:<9} {}", for_stdout(name), if *ok == true { "signed in" } else { "NOT signed in" });
                }
                match engines_line(&engines) {
                    Some(line) => println!("engines   {}", for_stdout(&line)),
                    None => println!("engines   not checked yet (the `engines` job runs scripts/update-engines.sh daily)"),
                }
                println!("model     {}", for_stdout(models["default"].as_str().unwrap_or("")));
                if !memory.is_null() {
                    println!("memory    {}", for_stdout(&memory_line(&memory)));
                }
                if !jobs.is_null() {
                    println!("jobs      {}", for_stdout(&jobs_summary(&jobs)));
                }
                let s1_errors = s1["errors"].as_u64().unwrap_or(0);
                let s1_files = if s1_errors > 0 { format!("  ({s1_errors} problem(s) in the decision files: zen s1 check)") } else { String::new() };
                match models["scorer"].as_str() {
                    None => println!("s1        off (set ZEN_S1_MODEL or write ~/.zenbot/s1/models.toml){s1_files}"),
                    Some(_) if models["authenticated"]["openrouter"] != true => {
                        println!("s1        {}  (NO key: set OPENROUTER_API_KEY){s1_files}", for_stdout(models["scorer"].as_str().unwrap_or("")))
                    }
                    Some(s) => println!("s1        {}{s1_files}", for_stdout(s)),
                }
                match &git_identity {
                    Some(id) => println!("git       {}", for_stdout(id)),
                    None => println!("git       no identity: zen's commits get a placeholder author (git config --global user.name/user.email)"),
                }
                let commit = for_stdout(health["commit"].as_str().filter(|c| !c.is_empty()).unwrap_or("unknown"));
                match (version["available"].as_bool(), version["latest"].as_str()) {
                    (Some(true), Some(latest)) => println!(
                        "version   {commit}  (update available: {}, {} new commit{}; run `zen upgrade`)",
                        for_stdout(latest),
                        version["behind"],
                        if version["behind"] == 1 { "" } else { "s" }
                    ),
                    (Some(false), _) => println!("version   {commit}  (up to date)"),
                    _ => println!("version   {commit}"),
                }
            }
            if status["ok"] != true {
                std::process::exit(1);
            }
        }
        Cmd::Memory { cmd: Some(MemoryCmd::Sleep), .. } => {
            let r = c.post("/api/memory/sleep", json!({})).await?;
            if json {
                out(&r);
            } else {
                println!("{}", for_stdout(&sleep_counts(&r)));
                if let Some(note) = r["note"].as_str() {
                    println!("{}", for_stdout(note));
                }
            }
        }
        Cmd::Skills { cmd: None } => {
            let s = c.get("/api/skills").await?;
            if json {
                out(&s);
            } else {
                for x in s["skills"].as_array().into_iter().flatten() {
                    // A draft a session that read untrusted content wrote: only the owner activates it.
                    let mark = if x["untrusted"] == true { ", untrusted" } else { "" };
                    let draft = if x["status"] == "draft" { format!(" (draft{}{mark})", short_sha(x)) } else { String::new() };
                    let last = x["last_load"].as_str().map(|t| format!(", last {}", t.get(..10).unwrap_or(t))).unwrap_or_default();
                    println!(
                        "{}{draft}  {}",
                        for_stdout(x["skill"].as_str().unwrap_or("")),
                        dim(&format!("{} loads{last}; accepted {} of {} judged sessions", x["loads"], x["sessions_accepted"], x["sessions_judged"]))
                    );
                }
                for t in s["tools"].as_array().into_iter().flatten() {
                    let state = if t["approved"] == true { "approved" } else { "sandboxed until approved" };
                    println!("tool made_{}{}  {}", for_stdout(t["tool"].as_str().unwrap_or("")), for_stdout(&short_sha(t)), dim(state));
                }
            }
        }
        Cmd::Skills { cmd: Some(r) } => {
            let (name, decision, sha) = r.parts();
            let res = c.post("/api/skills/review", json!({ "name": name, "decision": decision, "sha": sha })).await?;
            emit(json, &res, || res["result"].as_str().unwrap_or("").to_string());
        }
        Cmd::Policy { cmd: None } => {
            let p = c.get("/api/policy").await?;
            if json {
                out(&p);
            } else {
                println!("policy v{}: {}", p["version"], for_stdout(&serde_json::to_string(&p["policy"]).unwrap_or_default()));
                for s in p["stats"].as_array().into_iter().flatten() {
                    println!(
                        "  {:<10} {:<34} {}",
                        for_stdout(s["kind"].as_str().unwrap_or("?")),
                        for_stdout(s["model"].as_str().unwrap_or("?")),
                        dim(&format!("{} subtasks, {} of {} judged accepted, ${:.3} each", s["subtasks"], s["accepted"], s["judged"], s["mean_cost"].as_f64().unwrap_or(0.0)))
                    );
                }
                for s in p["suggestions"].as_array().into_iter().flatten() {
                    println!("suggests {} -> {} ({})", for_stdout(s["kind"].as_str().unwrap_or("")), for_stdout(s["model"].as_str().unwrap_or("")), for_stdout(s["why"].as_str().unwrap_or("")));
                }
            }
        }
        Cmd::Policy { cmd: Some(PolicyCmd::Undo) } => {
            let r = c.post("/api/policy/undo", json!({})).await?;
            emit(json, &r, || format!("policy v{}", r["version"]));
        }
        Cmd::Policy { cmd: Some(PolicyCmd::Set { kind, model, candidates, explore }) } => {
            let mut p = c.get("/api/policy").await?["policy"].clone();
            if !p["routes"].is_object() {
                p["routes"] = json!({});
            }
            let mut route = json!({ "model": model });
            if let Some(cands) = candidates {
                route["candidates"] = json!(cands.split(',').map(str::trim).filter(|x| !x.is_empty()).collect::<Vec<_>>());
            }
            p["routes"][&kind] = route;
            if let Some(e) = explore {
                p["explore"] = json!(e);
            }
            let r = c.post("/api/policy", json!({ "policy": p, "reason": format!("owner: {kind} -> {model}") })).await?;
            emit(json, &r, || format!("policy v{}", r["version"]));
        }
        Cmd::S1 { cmd } => s1(&c, cmd, json).await?,
        Cmd::Trust { cmd } => {
            let v = match cmd.unwrap_or(TrustCmd::List) {
                TrustCmd::List => c.get("/api/trust").await?,
                TrustCmd::Suggest => c.get("/api/trust?suggest=true").await?,
                TrustCmd::Add { kind, pattern, level, note } => c.post("/api/trust", json!({ "kind": kind, "match": pattern, "level": level, "note": note })).await?,
                TrustCmd::Rm { kind, pattern } => c.post("/api/trust", json!({ "kind": kind, "match": pattern, "remove": true })).await?,
            };
            emit(json, &v, || {
                if v["entries"].is_array() {
                    let mut s = String::new();
                    if let Some(p) = v["problem"].as_str() {
                        s.push_str(&format!("{}: {p} (the last good list is in force)\n", v["path"].as_str().unwrap_or("")));
                    }
                    for e in v["entries"].as_array().into_iter().flatten() {
                        s.push_str(&format!("{:<5} {:<40} {}{}\n", e["kind"].as_str().unwrap_or(""), e["match"].as_str().unwrap_or(""), e["level"].as_str().unwrap_or(""),
                            e["note"].as_str().filter(|n| !n.is_empty()).map(|n| format!("  ({n})")).unwrap_or_default()));
                    }
                    let built: Vec<&str> = v["built_in"].as_array().into_iter().flatten().filter_map(|e| e["match"].as_str()).collect();
                    s.push_str(&format!("built in: {}", built.join(", ")));
                    s
                } else {
                    format!("{} {} {}", v["kind"].as_str().unwrap_or(""), v["match"].as_str().unwrap_or(""), v["level"].as_str().unwrap_or("removed"))
                }
            });
        }
        Cmd::Tools { cmd } => {
            let (name, decision, sha) = cmd.parts();
            let res = c.post(&format!("/api/tools/{}/review", enc(name.trim_start_matches("made_"))), json!({ "decision": decision, "sha": sha })).await?;
            emit(json, &res, || res["result"].as_str().unwrap_or("").to_string());
        }
        Cmd::Jobs { cmd: None } => {
            let r = c.get("/api/jobs").await?;
            if json {
                out(&r);
            } else {
                if r["scheduler"] == false {
                    println!("{}", dim("the scheduler is off in this kernel (ZEN_JOBS=0)"));
                }
                for j in r["jobs"].as_array().into_iter().flatten() {
                    println!("{}", for_stdout(&job_line(j)));
                }
            }
        }
        Cmd::Jobs { cmd: Some(cmd) } => match cmd {
            JobsCmd::Add { name, schedule, prompt, tz, context, skills, model, dir, report_file, post_to } => {
                let body = json!({ "name": name, "schedule": schedule, "prompt": prompt, "tz": tz, "context": context, "skills": skills, "model": model, "workspace": dir, "report_file": report_file, "post_to": post_to });
                let j = c.post("/api/jobs", body).await?;
                emit(json, &j, || job_line(&j));
            }
            JobsCmd::Set { name, schedule, prompt, tz, context, skills, model, report_file, post_to } => {
                let body = json!({ "schedule": schedule, "prompt": prompt, "tz": tz, "context": context, "skills": skills, "model": model, "report_file": report_file, "post_to": post_to });
                let j = c.patch(&format!("/api/jobs/{}", enc(&name)), body).await?;
                emit(json, &j, || job_line(&j));
            }
            JobsCmd::Pause { name } => {
                let j = c.patch(&format!("/api/jobs/{}", enc(&name)), json!({ "enabled": false })).await?;
                emit(json, &j, || job_line(&j));
            }
            JobsCmd::Resume { name } => {
                let j = c.patch(&format!("/api/jobs/{}", enc(&name)), json!({ "enabled": true })).await?;
                emit(json, &j, || job_line(&j));
            }
            JobsCmd::Rm { name } => {
                let r = c.delete(&format!("/api/jobs/{}", enc(&name))).await?;
                emit(json, &r, || format!("removed {name}"));
            }
            JobsCmd::Run { name } => {
                let r = c.post(&format!("/api/jobs/{}/run", enc(&name)), json!({})).await?;
                emit(json, &r, || format!("started run {} of {name}; see `zen jobs runs {name}`", r["run"]));
            }
            JobsCmd::Runs { name, limit } => {
                let q = name.as_deref().map(|n| format!("&job={}", enc(n))).unwrap_or_default();
                let r = c.get(&format!("/api/jobs/runs?limit={limit}{q}")).await?;
                if json {
                    out(&r);
                } else {
                    for x in r.as_array().into_iter().flatten() {
                        let when = x["started_at"].as_str().map(|t| t.get(..16).unwrap_or(t).replace('T', " ")).unwrap_or_default();
                        let session = x["session"].as_str().map(|s| format!(", session {}", short(s))).unwrap_or_default();
                        println!("{} {}  {}", for_stdout(x["job"].as_str().unwrap_or("")), when, dim(&format!("{} ({}{session})", x["status"].as_str().unwrap_or(""), x["trigger"].as_str().unwrap_or(""))));
                        if let Some(body) = x["output"].as_str().or(x["error"].as_str()) {
                            for line in body.lines().take(12) {
                                println!("  {}", for_stdout(line));
                            }
                        }
                    }
                }
            }
        },
        Cmd::Suggestions => {
            let st = c.get("/api/suggestions").await?;
            if json {
                out(&st);
            } else {
                print_suggestions(&st);
            }
        }
        Cmd::Brain { action, id, args } => {
            let mut a: Value = match args.as_deref() {
                Some("-") => {
                    let mut s = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
                    serde_json::from_str(&s)?
                }
                Some(text) => serde_json::from_str(text)?,
                None => json!({}),
            };
            anyhow::ensure!(a.is_object(), "--args must be a JSON object");
            a["action"] = json!(action);
            if let Some(id) = id {
                a["id"] = json!(id);
            }
            out(&c.post("/api/brain", a).await?);
        }
        Cmd::Memory { cmd: None, tier } => {
            let m = c.get(&format!("/api/memory?tier={}", enc(&tier))).await?;
            if json {
                out(&m);
            } else {
                for x in m["memories"].as_array().into_iter().flatten() {
                    let tier = if x["tier"] == "short" { String::new() } else { format!(", {}", x["tier"].as_str().unwrap_or("")) };
                    let mark = if x["untrusted"] == true { ", untrusted" } else { "" };
                    println!("{:<6} {}  {}", for_stdout(x["id"].as_str().unwrap_or("")), for_stdout(x["text"].as_str().unwrap_or("")), dim(&format!("({}{tier}{mark})", x["source"].as_str().unwrap_or(""))));
                }
                println!("{}", dim(&memory_line(&m)));
            }
        }
        Cmd::Sessions(cmd) => match cmd {
            SessionsCmd::Ls { archived } => {
                let list = c.get(&format!("/api/sessions?archived={archived}")).await?;
                if json { out(&list) } else { print_sessions(&list) }
            }
            SessionsCmd::New { model, effort, title } => {
                let s = c.post("/api/sessions", json!({ "model": model, "effort": effort, "title": title })).await?;
                emit(json, &s, || s["id"].as_str().unwrap_or("").to_string());
            }
            SessionsCmd::Show { id } => {
                let (_, s) = session_op(&c, &id, reqwest::Method::GET, None).await?;
                if json { out(&s) } else { print_messages(&s) }
            }
            SessionsCmd::Archive { id } => {
                let (id, s) = session_op(&c, &id, reqwest::Method::PATCH, Some(json!({ "archived": true }))).await?;
                emit(json, &s, || format!("archived {}", short(&id)));
            }
            SessionsCmd::Restore { id } => {
                let (id, s) = session_op(&c, &id, reqwest::Method::PATCH, Some(json!({ "archived": false }))).await?;
                emit(json, &s, || format!("restored {}", short(&id)));
            }
            SessionsCmd::Decide { id, decision, note } => {
                let id = c.resolve(&id).await?;
                let d = c.post(&format!("/api/sessions/{id}/decision"), json!({ "decision": decision, "note": note })).await?;
                emit(json, &d, || format!("recorded {} for {}", d["decision"].as_str().unwrap_or(""), short(&id)));
            }
            SessionsCmd::Rename { id, title } => {
                let (id, s) = session_op(&c, &id, reqwest::Method::PATCH, Some(json!({ "title": title }))).await?;
                emit(json, &s, || format!("renamed {}", short(&id)));
            }
        },
    }
    Ok(())
}

/// Resolve a session id or unique prefix, then call `/api/sessions/<id>` with `method`.
async fn session_op(c: &Client, id: &str, method: reqwest::Method, body: Option<Value>) -> Result<(String, Value)> {
    let id = c.resolve(id).await?;
    let v = c.call(method, &format!("/api/sessions/{id}"), body).await?;
    Ok((id, v))
}

/// `zen s1 …`: System One's decisions, files, models, replay, compare and proposals.
async fn s1(c: &Client, cmd: Option<S1Cmd>, json: bool) -> anyhow::Result<()> {
    match cmd {
        None => {
            let v = c.get("/api/s1").await?;
            if json {
                emit(true, &v, String::new);
                return Ok(());
            }
            let files = &v["files"];
            match files["models"]["default"].as_str() {
                Some(d) => println!("System One: default model `{}` ({})", for_stdout(d), for_stdout(files["models"]["source"].as_str().unwrap_or(""))),
                None => println!("System One is off: set ZEN_S1_MODEL or write ~/.zenbot/s1/models.toml"),
            }
            for d in v["decisions"].as_array().into_iter().flatten() {
                let outcomes: Vec<String> = d["outcomes"].as_array().into_iter().flatten()
                    .map(|o| format!("{} {}", o["outcome"].as_str().unwrap_or("-"), o["count"])).collect();
                let latency = match (d["latency_ms"]["p50"].as_f64(), d["latency_ms"]["p95"].as_f64()) {
                    (Some(a), Some(b)) => format!("{a:.0}/{b:.0} ms"),
                    _ => "-".into(),
                };
                let labelled = d["labelled"].as_i64().unwrap_or(0);
                let right = if labelled > 0 { format!(", right {} of {labelled}", d["correct"]) } else { String::new() };
                println!("  {:<16} {:>5} calls  {}", for_stdout(d["decision"].as_str().unwrap_or("?")), d["calls"],
                    dim(&format!("{} unavailable, {} blocked, {latency}, ${:.4}{right}  {}", d["unavailable"], d["blocked"], d["cost_usd"].as_f64().unwrap_or(0.0), outcomes.join(", "))));
            }
            let errors = files["errors"].as_u64().unwrap_or(0);
            if errors > 0 {
                println!("{errors} problem(s) in the decision files: zen s1 check");
            }
            if v["proposals"].as_u64().unwrap_or(0) > 0 {
                println!("{} proposal(s) from the learner: zen s1 proposals", v["proposals"]);
            }
        }
        Some(S1Cmd::Check) => {
            let v = c.get("/api/s1/check").await?;
            let errors = v["errors"].as_u64().unwrap_or(0);
            if json {
                emit(true, &v, String::new);
            } else {
                for p in v["problems"].as_array().into_iter().flatten() {
                    for d in p["diags"].as_array().into_iter().flatten() {
                        println!("{}:{}:{}  error  {}", for_stdout(p["path"].as_str().unwrap_or("")), d["line"], d["col"], for_stdout(d["message"].as_str().unwrap_or("")));
                    }
                }
                let files = v["files"].as_array().map(|a| a.len()).unwrap_or(0);
                let own = v["files"].as_array().into_iter().flatten().filter(|f| f["source"] != "built-in").count();
                println!("{files} decisions in force ({own} from your files), {errors} error(s)");
            }
            if errors > 0 {
                std::process::exit(1);
            }
        }
        Some(S1Cmd::Models) => {
            let v = c.get("/api/s1/models").await?;
            emit(json, &v, || {
                let mut s = format!("approved models ({}):", v["source"].as_str().unwrap_or(""));
                for (name, m) in v["list"].as_object().into_iter().flatten() {
                    let default = if v["default"].as_str() == Some(name) { "  (default)" } else { "" };
                    s.push_str(&format!("\n  {name:<12} {}{default}  {}", m["id"].as_str().unwrap_or(""), m["note"].as_str().unwrap_or("")));
                }
                s
            });
        }
        Some(S1Cmd::Replay { decision, file, last }) => {
            let file = match file {
                Some(p) => Some(std::fs::read_to_string(&p).map_err(|e| anyhow::anyhow!("reading {}: {e}", p.display()))?),
                None => None,
            };
            let v = c.post("/api/s1/replay", json!({ "decision": decision, "file": file, "last": last })).await?;
            emit(json, &v, || {
                let mut s = format!("{}: {} recorded decisions, {} would change ({} to the right outcome, {} away from it)",
                    v["decision"].as_str().unwrap_or(""), v["replayed"], v["changed"], v["now_right"], v["now_wrong"]);
                for e in v["examples"].as_array().into_iter().flatten() {
                    s.push_str(&format!("\n  #{}: {} -> {}", e["id"], e["was"].as_str().unwrap_or("-"), e["now"].as_str().unwrap_or("-")));
                }
                s
            });
        }
        Some(S1Cmd::Compare { decision, models, last }) => {
            let v = c.post("/api/s1/compare", json!({ "decision": decision, "models": models, "last": last })).await?;
            emit(json, &v, || {
                let mut s = format!("{}: {} recorded states", v["decision"].as_str().unwrap_or(""), v["states"]);
                for m in v["models"].as_array().into_iter().flatten() {
                    s.push_str(&format!("\n  {:<12} agrees {} of {}, right {} of {} labelled, {} ms, ${:.4}{}", m["model"].as_str().unwrap_or(""),
                        m["agree_with_decided"], m["asked"], m["right"], m["labelled"], m["mean_ms"], m["cost_usd"].as_f64().unwrap_or(0.0),
                        if m["failed"].as_u64().unwrap_or(0) > 0 { format!(", {} failed", m["failed"]) } else { String::new() }));
                }
                s
            });
        }
        Some(S1Cmd::Proposals) => {
            let v = c.get("/api/s1/proposals").await?;
            emit(json, &v, || {
                let items = v.as_array().cloned().unwrap_or_default();
                if items.is_empty() {
                    return "no proposals".into();
                }
                items.iter().map(|p| format!("#{} {}  (zen s1 approve {} | zen s1 reject {})", p["id"], p["summary"].as_str().unwrap_or(""), p["id"], p["id"])).collect::<Vec<_>>().join("\n")
            });
        }
        Some(S1Cmd::Approve { id }) => {
            let v = c.post(&format!("/api/s1/proposals/{id}"), json!({ "decision": "approve" })).await?;
            emit(json, &v, || format!("proposal {id} applied to ~/.zenbot/s1/{}.toml", v["decision"].as_str().unwrap_or("")));
        }
        Some(S1Cmd::Reject { id }) => {
            let v = c.post(&format!("/api/s1/proposals/{id}"), json!({ "decision": "reject" })).await?;
            emit(json, &v, || format!("proposal {id} declined"));
        }
    }
    Ok(())
}
