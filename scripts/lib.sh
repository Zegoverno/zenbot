#!/usr/bin/env bash
# Helpers shared by the scripts in this directory (and install.sh). Source it; it only defines
# functions and works under `set -euo pipefail`.

# A setting from the service's environment file, ~/.zenbot/env (empty when unset there).
zen_env() { { grep -E "^$1=" "$HOME/.zenbot/env" 2>/dev/null || true; } | tail -1 | cut -d= -f2-; }

# Set KEY=VALUE in ~/.zenbot/env, replacing any line for KEY. The file is rewritten whole (private,
# then renamed over the old one).
env_set() {
  local env="$HOME/.zenbot/env" tmp
  mkdir -p "$HOME/.zenbot"
  tmp=$(umask 077 && mktemp "$env.XXXXXX")
  { grep -vE "^$1=" "$env" 2>/dev/null || true; echo "$1=$2"; } > "$tmp"
  chmod 600 "$tmp" && mv -f "$tmp" "$env"
}

# Keep a copy of ~/.zenbot/env as ~/.zenbot/env.prev (private; the kernel treats it as secret like
# env itself) before a script rewrites settings the owner didn't pass to it.
env_backup() { [ ! -f "$HOME/.zenbot/env" ] || cp -p "$HOME/.zenbot/env" "$HOME/.zenbot/env.prev"; }

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

# The systemd timers older versions installed (engine updates, the memory sleep), stopped and
# removed: the kernel's scheduler runs both now, as system jobs (D-046). install.sh and
# apply-upgrade.sh (after the new kernel is healthy) call it; it does nothing when they're gone.
remove_old_timers() {
  local unit removed=
  for unit in zen-engines.timer zen-engines.service zen-sleep.timer zen-sleep.service; do
    [ -e "/etc/systemd/system/$unit" ] || continue
    sudo systemctl disable --now "$unit" >/dev/null 2>&1 || true
    sudo rm -f "/etc/systemd/system/$unit"
    removed=1
  done
  [ -z "$removed" ] || sudo systemctl daemon-reload
}

# trim_target DIR [LIMIT_BYTES]: keep a cargo target directory under LIMIT (default 2 GiB). Over it,
# first the incremental caches and target/debug go (every script builds --release), then, if still
# over, the whole directory (`cargo clean`'s effect; the next build compiles from scratch). Only
# directories cargo made (with its CACHEDIR.TAG) are touched, and nothing while a cargo process
# runs. Prints what it did; apply-upgrade.sh calls it after a successful install.
trim_target() {
  local dir=$1 limit=${2:-$((2 << 30))}
  [ -f "$dir/CACHEDIR.TAG" ] || return 0
  (($(du -sb "$dir" | cut -f1) > limit)) || return 0
  if pgrep -x cargo >/dev/null; then echo "$dir is over $((limit >> 20)) MiB, but cargo is running; left alone"; return 0; fi
  rm -rf "$dir"/debug "$dir"/*/incremental
  if (($(du -sb "$dir" | cut -f1) > limit)); then
    rm -rf "${dir:?}"
    echo "$dir was still over $((limit >> 20)) MiB without its incremental caches: removed it"
  else
    echo "$dir was over $((limit >> 20)) MiB: removed its incremental caches and debug build ($(($(du -sb "$dir" | cut -f1) >> 20)) MiB left)"
  fi
}

# Dangling Docker images and build cache unused for a week, removed; volumes and images a container
# uses are never touched. Failures are ignored. apply-upgrade.sh calls it after a successful install.
docker_prune() {
  sudo docker image prune -f 2>&1 | tail -1 || true
  sudo docker builder prune -f --filter until=168h 2>&1 | tail -1 || true
}
