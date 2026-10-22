#!/usr/bin/env bash
# SearXNG, natively (D-078): zenbot's free web search, without Docker or an API key. One Python
# environment per upstream commit under ~/.zenbot/searxng/releases/<sha>, `current` pointing at the
# one in use, settings from deploy/searxng/settings.yml (127.0.0.1:8888, JSON on, no limiter or
# image proxy) with a secret key made on this machine, and a systemd service (deploy/searxng.service).
#
#   scripts/searxng.sh install          install the pinned commit (or keep a newer one) and start it
#   scripts/searxng.sh update           move to upstream's latest commit: install it beside the
#                                       current one, run a real search on a spare port, switch, and
#                                       go back if the service doesn't answer (the `engines` job)
#   scripts/searxng.sh check [port]     one real search; exit 0 when it returns results
#
# A search is the check: engines (Google, DuckDuckGo, Brave, Bing, Startpage, Wikipedia, …) block
# now and then, and SearXNG answers with what the others found.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$REPO/scripts/lib.sh"

# The commit installs start from (bump deliberately; the nightly update moves past it).
PINNED=6671d89bede8c9fc108b17bb98916170f5657650
UPSTREAM=https://github.com/searxng/searxng
ROOT="$HOME/.zenbot/searxng"
SETTINGS="$ROOT/settings.yml"
PORT=8888

check() { # check PORT: a JSON search with results
  local port=${1:-$PORT} out
  out=$(curl -fsS --max-time 20 "http://127.0.0.1:$port/search?q=linux+kernel&format=json" 2>/dev/null) || return 1
  jq -e '.results | length > 0' >/dev/null <<<"$out"
}

installed() { [ -L "$ROOT/current" ] && basename "$(readlink "$ROOT/current")" || true; }

# The settings file: the repo's, with this machine's own secret key (made once, kept).
write_settings() {
  local key
  key=$(sed -n 's/^  secret_key: "\(.*\)"$/\1/p' "$SETTINGS" 2>/dev/null || true)
  [ -n "$key" ] && [ "$key" != "zenbot-local-only-change-me" ] || key=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
  mkdir -p "$ROOT"
  sed "s/^  secret_key: \".*\"$/  secret_key: \"$key\"/" "$REPO/deploy/searxng/settings.yml" > "$SETTINGS.new"
  chmod 600 "$SETTINGS.new" && mv -f "$SETTINGS.new" "$SETTINGS"
}

# Build releases/SHA (a checkout of that commit and its own venv), marked complete only at the
# end; a failed build leaves nothing behind.
build() {
  local sha=$1 dir="$ROOT/releases/$1"
  [ -f "$dir/.complete" ] && return 0
  rm -rf "$dir" && mkdir -p "$dir"
  if git -C "$dir" init -q && git -C "$dir" fetch -q --depth 1 "$UPSTREAM" "$sha" && git -C "$dir" checkout -q FETCH_HEAD \
    && python3 -m venv "$dir/venv" \
    && "$dir/venv/bin/pip" install -q -U pip setuptools wheel \
    && "$dir/venv/bin/pip" install -q -r "$dir/requirements.txt" \
    && "$dir/venv/bin/pip" install -q --use-pep517 --no-build-isolation -e "$dir"; then
    touch "$dir/.complete"
  else
    rm -rf "$dir"; echo "building SearXNG ${sha:0:12} failed" >&2; return 1
  fi
}

# Run releases/SHA on a spare port and search through it.
trial() {
  local sha=$1 port=18888 pid ok=1
  SEARXNG_SETTINGS_PATH="$SETTINGS" SEARXNG_PORT=$port "$ROOT/releases/$sha/venv/bin/python" -m searx.webapp >/dev/null 2>&1 &
  pid=$!
  for _ in $(seq 1 30); do check $port && { ok=0; break; }; sleep 1; done
  kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
  return $ok
}

service() {
  sed -e "s#__USER__#$USER#g" -e "s#__HOME__#$HOME#g" "$REPO/deploy/searxng.service" | sudo tee /etc/systemd/system/zen-searxng.service >/dev/null
  sudo systemctl daemon-reload
  sudo systemctl enable zen-searxng >/dev/null 2>&1
}

# The Docker SearXNG of earlier installs holds port 8888: stop it for good.
stop_docker_searxng() {
  command -v docker >/dev/null || return 0
  local c
  for c in $(sudo -n docker ps -aq --filter "publish=8888" 2>/dev/null); do
    sudo -n docker update --restart=no "$c" >/dev/null 2>&1 || true
    sudo -n docker stop "$c" >/dev/null 2>&1 || true
  done
}

switch_to() { # switch_to SHA: point current at it, restart, check; false if it doesn't answer
  ln -sfn "releases/$1" "$ROOT/current.new" && mv -fT "$ROOT/current.new" "$ROOT/current"
  sudo systemctl restart zen-searxng
  for _ in $(seq 1 30); do check && return 0; sleep 1; done
  return 1
}

prune() { # keep the current release and one before it
  local cur keep=0 d
  cur=$(installed)
  for d in $(ls -1t "$ROOT/releases" 2>/dev/null); do
    [ "$d" = "$cur" ] && continue
    keep=$((keep + 1)); [ $keep -le 1 ] && continue
    rm -rf "${ROOT:?}/releases/$d"
  done
}

case "${1:-}" in
  install)
    mkdir -p "$ROOT/releases"
    write_settings
    have=$(installed)
    target=${have:-$PINNED}
    echo "== SearXNG ${target:0:12}"
    build "$target"
    service
    stop_docker_searxng
    switch_to "$target" || { journalctl -u zen-searxng -n 20 --no-pager; echo "SearXNG doesn't answer on 127.0.0.1:$PORT" >&2; exit 1; }
    echo "SearXNG ${target:0:12} answers on 127.0.0.1:$PORT"
    ;;
  update)
    have=$(installed)
    [ -n "$have" ] || { echo "searxng: not installed (scripts/searxng.sh install)"; exit 0; }
    latest=$(git ls-remote "$UPSTREAM" refs/heads/master 2>/dev/null | cut -f1)
    [[ "$latest" =~ ^[0-9a-f]{40}$ ]] || { echo "searxng ${have:0:12}: latest commit unknown; skipped"; exit 0; }
    [ "$latest" != "$have" ] || { echo "searxng ${have:0:12}: up to date"; exit 0; }
    write_settings
    if ! build "$latest" || ! trial "$latest"; then
      rm -rf "${ROOT:?}/releases/$latest"
      echo "searxng ${have:0:12} → ${latest:0:12} ROLLED BACK: the new version didn't answer a test search"; exit 1
    fi
    if switch_to "$latest"; then
      prune
      echo "searxng ${have:0:12} → ${latest:0:12} OK"
    else
      switch_to "$have" || true
      rm -rf "${ROOT:?}/releases/$latest"
      echo "searxng ${have:0:12} → ${latest:0:12} ROLLED BACK: the service didn't answer after switching"; exit 1
    fi
    ;;
  check) check "${2:-$PORT}" && echo "searxng: answers" || { echo "searxng: no results on 127.0.0.1:${2:-$PORT}"; exit 1; } ;;
  *) echo "usage: $0 install | update | check [port]" >&2; exit 2 ;;
esac
