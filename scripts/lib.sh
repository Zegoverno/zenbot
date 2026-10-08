#!/usr/bin/env bash
# Helpers shared by the scripts in this directory (and install.sh). Source it; it only defines
# functions and works under `set -euo pipefail`.

# A setting from the service's environment file, ~/.zenbot/env (empty when unset there).
zen_env() { { grep -E "^$1=" "$HOME/.zenbot/env" 2>/dev/null || true; } | tail -1 | cut -d= -f2-; }

# wait_healthy URL SECS [PID]: wait until URL (a kernel's /health) reports ok. Fails after SECS
# seconds, or as soon as process PID, when given, has exited.
wait_healthy() {
  local url=$1 secs=$2 pid=${3:-} i
  for ((i = 0; i < secs; i++)); do
    curl -fs "$url" 2>/dev/null | grep -q '"ok":true' && return 0
    [ -z "$pid" ] || kill -0 "$pid" 2>/dev/null || return 1
    sleep 1
  done
  curl -fs "$url" 2>/dev/null | grep -q '"ok":true'
}

# Rust and a C toolchain, installed only when cargo is missing (prebuilt binaries need neither).
ensure_rust() {
  export PATH="$HOME/.cargo/bin:$PATH"
  command -v cargo >/dev/null && return 0
  echo "installing Rust to build from source"
  local need=() p
  for p in build-essential pkg-config; do dpkg -s "$p" >/dev/null 2>&1 || need+=("$p"); done
  [ ${#need[@]} -eq 0 ] || sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${need[@]}"
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
}

# The API token in ~/.zenbot/token, created (random, private) when there is none yet.
new_token() {
  mkdir -p "$HOME/.zenbot"
  [ -s "$HOME/.zenbot/token" ] || (umask 077 && head -c 24 /dev/urandom | base64 | tr -d '/+=' > "$HOME/.zenbot/token")
  chmod 600 "$HOME/.zenbot/token"
}

# The timers in deploy/ (engine updates, the memory sleep), written to /etc/systemd/system and
# enabled. install.sh calls it, and apply-upgrade.sh, so an upgrade brings a new timer too.
install_timers() {
  local repo=$1 user=${USER:-$(id -un)} unit
  for unit in zen-engines.service zen-engines.timer zen-sleep.service zen-sleep.timer; do
    sed -e "s#__USER__#$user#g" -e "s#__REPO__#$repo#g" -e "s#__HOME__#$HOME#g" "$repo/deploy/$unit" \
      | sudo tee "/etc/systemd/system/$unit" >/dev/null
  done
  sudo systemctl daemon-reload
  # Daily: the Claude Code and Codex CLIs on their latest versions, tested (scripts/update-engines.sh).
  sudo systemctl enable --now zen-engines.timer >/dev/null 2>&1
  # Nightly: short-term memory tidied to its size (scripts/sleep.sh).
  sudo systemctl enable --now zen-sleep.timer >/dev/null 2>&1
}
