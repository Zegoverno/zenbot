# zenbot

A personal + company operating system for niche builders: a machine for thinking, analyzing and building, with LLMs at the center and agents doing the work.

Status: design phase. See [SPEC.md](SPEC.md).

## Run (walking skeleton)

Requirements: Docker, Rust, Node 22+.

```bash
cd packages/mind && npm install && cd ../..
mkdir -p ~/.zenbot && (cd ~/.zenbot && npx --prefix ../zenbot/packages/mind pi-ai login openai)   # Sign in with ChatGPT
./scripts/dev.sh            # Postgres + zend on :8100 (ZEN_FAUX=1 adds a scripted test model)
cat ~/.zenbot/token         # access token for the web UI
```

Open `http://<host>:8100/?token=<token>` once; the token is remembered in the browser.
