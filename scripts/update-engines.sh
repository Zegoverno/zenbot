#!/usr/bin/env bash
# Keep the model engines on their latest versions: the Claude Code CLI, the Codex CLI and, when
# the `pi` worker is enabled, Pi (@earendil-works/pi-ai and pi-agent-core in packages/mind).
# Run daily by zen-engines.timer (deploy/); safe to run by hand, also from inside a zen session.
#
#   scripts/update-engines.sh           update what is behind, test it, roll back what fails
#   scripts/update-engines.sh --check   report installed and latest versions; change nothing
#
# Each update is tested and undone if the test fails:
# - Claude Code and Codex: the new release is downloaded from the vendor's own channel and its
#   SHA-256 checked against the vendor's manifest. It is then installed next to the old one, which
#   is kept, wherever the CLI lives now (a plain binary such as /usr/local/bin/claude, the native or
#   standalone installers' versioned layout, or a global npm install). Then one real tool-free
#   completion runs through zen-engine (`complete`, the path the kernel uses for summaries). If the
#   check fails, the previous version is restored. Both CLIs are started fresh for every turn,
#   so no restart is needed. When the check already fails on the installed version (signed out,
#   offline), the update is skipped instead, since a test that fails anyway can't judge it.
# - Pi: the latest release is installed into packages/mind/node_modules only (npm --no-save). The
#   repo pins Pi's minimum version and this job makes no commits; scripts/mind-deps.sh keeps the
#   newer Pi when zenbot is reinstalled. It is checked: ping, the scripted model, and a real System
#   One call when ZEN_S1_MODEL is set. Then it is applied with scripts/upgrade.sh, which runs a
#   scripted Pi turn through a second kernel and restarts zenbot once no session is working. A
#   failure puts the previous Pi back. Pi is skipped when the checkout has local changes or isn't
#   on main, since upgrade.sh installs whatever the checkout holds.
#
# Results go to ~/.zenbot/upgrade.log ("engines: …", one line per engine) and the latest state
# to ~/.zenbot/engines.json (shown by `zen status`).
# ZEN_ENGINES=claude,codex,pi limits which engines are looked at. ZEN_ENGINES_FAIL=<engine> makes
# that engine's post-update check fail, to test the rollback.
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="$HOME/.local/bin:$HOME/.local/node/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin:$PATH"
ZEN="$HOME/.zenbot"
LOG="$ZEN/upgrade.log"
STATE="$ZEN/engines.json"
CHECK_ONLY=
case ${1:-} in
  --check) CHECK_ONLY=1 ;;
  "") ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac
mkdir -p "$ZEN"

exec 9>"$ZEN/engines.lock"
flock -n 9 || { echo "another engine update is running"; exit 0; }

CLAUDE_DL=https://downloads.claude.ai/claude-code-releases
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

say() { echo "$*"; }
log() { [ -n "$CHECK_ONLY" ] || echo "$(date -u +%FT%TZ) engines: $*" >> "$LOG"; say "$*"; }
# newer A B: B is a higher version than A.
newer() { [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)" = "$2" ]; }
is_version() { [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; }
wanted() { [ -z "${ZEN_ENGINES:-}" ] || [[ ",$ZEN_ENGINES," == *",$1,"* ]]; }
forced_fail() { [[ ",${ZEN_ENGINES_FAIL:-}," == *",$1,"* ]]; }
pi_enabled() { grep -qE '^ZEN_WORKERS=.*pi' "$ZEN/env" 2>/dev/null; }
# Run a command as root only when the target directory isn't ours to write.
as_owner() { local dir=$1; shift; if [ -w "$dir" ]; then "$@"; else sudo -n "$@"; fi; }
# Point symlink $1 at $2 atomically.
relink() { local tmp; tmp="$(dirname "$1")/.zen-relink.$$"; as_owner "$(dirname "$1")" ln -sfn "$2" "$tmp" && as_owner "$(dirname "$1")" mv -fT "$tmp" "$1"; }

declare -A STATUS VERSION
record() { VERSION[$1]=$2; STATUS[$1]=$3; }
# rolled_back ENGINE NOW OLD NEW WHY: the update OLD → NEW failed its check and was undone.
rolled_back() { record "$1" "$2" "rolled back from $4"; log "$1 $3 → $4 ROLLED BACK to $2: $5"; }

# ---- the check every CLI update must pass: a real completion through zen-engine ----

zen_engine() {
  local b
  for b in "$ZEN/bin/zen-engine" "$REPO/target/release/zen-engine"; do [ -x "$b" ] && { echo "$b"; return 0; }; done
  return 1
}

# The cheapest-looking model the engine lists (override with ZEN_ENGINES_CLAUDE_MODEL/_CODEX_MODEL).
check_model() {
  local engine=$1 over ids
  over="ZEN_ENGINES_${engine^^}_MODEL"
  [ -n "${!over:-}" ] && { echo "${!over}"; return; }
  ids=$(echo '{"jsonrpc":"2.0","id":1,"method":"models.list","params":{}}' | timeout 90 "$(zen_engine)" 2>/dev/null | head -1 | jq -r '.result.models[]?.id' 2>/dev/null)
  case $engine in
    claude) { echo "$ids" | grep -m1 '^claude/.*haiku' || echo "$ids" | grep -m1 '^claude/'; } ;;
    codex) echo "$ids" | grep '^codex/' | tail -1 ;;
  esac
}

# cli_check ENGINE VERSION: the CLI reports VERSION and answers one real tool-free completion.
# Prints why it failed.
cli_check() {
  local engine=$1 want=$2 have model req res
  if forced_fail "$engine"; then echo "forced failure (ZEN_ENGINES_FAIL)"; return 1; fi
  have=$("${engine}_installed")
  [ "$have" = "$want" ] || { echo "\`$engine --version\` says ${have:-nothing}, expected $want"; return 1; }
  zen_engine >/dev/null || { echo "no zen-engine binary to test with"; return 1; }
  model=$(check_model "$engine")
  [ -n "$model" ] || { echo "zen-engine lists no $engine model (signed out?)"; return 1; }
  req=$(jq -nc --arg m "$model" '{jsonrpc:"2.0",id:1,method:"complete",params:{model:$m,system:"You are a health check.",prompt:"Reply with exactly: OK"}}')
  res=$(cd "$TMP" && echo "$req" | timeout 180 "$(zen_engine)" 2>/dev/null | head -1)
  if echo "$res" | jq -e '.result.error == null and (.result.text // "" | test("OK"))' >/dev/null 2>&1; then return 0; fi
  echo "a test turn on $model failed: $(echo "$res" | jq -r '.result.error // .error.message // "no answer"' 2>/dev/null | head -c 300)"
  return 1
}

# ---- Claude Code ----

claude_installed() { claude --version 2>/dev/null | awk 'NR==1{print $1}'; }
claude_latest() { curl -fsSL --max-time 30 "$CLAUDE_DL/latest" 2>/dev/null; }
claude_platform() {
  local arch
  case "$(uname -m)" in x86_64|amd64) arch=x64 ;; arm64|aarch64) arch=arm64 ;; *) return 1 ;; esac
  if [ -f /lib/libc.musl-x86_64.so.1 ] || [ -f /lib/libc.musl-aarch64.so.1 ] || ldd /bin/ls 2>&1 | grep -q musl; then echo "linux-$arch-musl"; else echo "linux-$arch"; fi
}

# Install Claude Code $1 in place of the current one; sets ROLLBACK to the command that undoes it.
claude_install() {
  local new=$1 path real platform sum bin dir old
  path=$(command -v claude) || return 1
  real=$(readlink -f "$path")
  platform=$(claude_platform) || { echo "unsupported platform $(uname -m)"; return 1; }
  sum=$(curl -fsSL --max-time 30 "$CLAUDE_DL/$new/manifest.json" | jq -r --arg p "$platform" '.platforms[$p].checksum // empty')
  [[ "$sum" =~ ^[a-f0-9]{64}$ ]] || { echo "no checksum for $platform in the $new manifest"; return 1; }
  bin="$TMP/claude-$new"
  curl -fsSL --max-time 600 -o "$bin" "$CLAUDE_DL/$new/$platform/claude" || { echo "download failed"; return 1; }
  [ "$(sha256sum "$bin" | cut -d' ' -f1)" = "$sum" ] || { echo "checksum mismatch"; return 1; }
  chmod 755 "$bin"
  [ "$("$bin" --version 2>/dev/null | awk 'NR==1{print $1}')" = "$new" ] || { echo "the downloaded binary doesn't run"; return 1; }
  dir=$(dirname "$real")
  if [ -L "$path" ]; then
    # Versioned layout (native installer: ~/.local/bin/claude -> …/claude/versions/X.Y.Z).
    old=$(readlink "$path")
    as_owner "$dir" install -m 755 "$bin" "$dir/$new" || return 1
    relink "$path" "$dir/$new" || return 1
    ROLLBACK="relink '$path' '$old'"
    CLEANUP="claude_prune '$dir' '$new' '$(basename "$real")'"
  else
    # A plain binary: keep the old one as .prev; rename over it so running sessions keep theirs.
    as_owner "$dir" cp -f "$real" "$real.prev" || return 1
    as_owner "$dir" install -m 755 "$bin" "$real.new" && as_owner "$dir" mv -f "$real.new" "$real" || return 1
    ROLLBACK="as_owner '$dir' cp -f '$real.prev' '$real.new' && as_owner '$dir' mv -f '$real.new' '$real'"
    CLEANUP=:
  fi
}
# Keep only the current and previous version in a versioned layout.
claude_prune() { local f; for f in "$1"/*; do case "$(basename "$f")" in "$2"|"$3") ;; [0-9]*.[0-9]*.[0-9]*) as_owner "$1" rm -f "$f" ;; esac; done; }

# ---- Codex ----

codex_installed() { codex --version 2>/dev/null | awk 'NR==1{print $NF}'; }
codex_latest() {
  local v
  v=$(curl -fsSL --max-time 30 https://api.github.com/repos/openai/codex/releases/latest 2>/dev/null | jq -r '.tag_name // empty' | sed 's/^rust-v//')
  is_version "$v" || v=$(npm view @openai/codex version 2>/dev/null)
  echo "$v"
}
codex_target() { case "$(uname -m)" in x86_64|amd64) echo x86_64-unknown-linux-musl ;; arm64|aarch64) echo aarch64-unknown-linux-musl ;; *) return 1 ;; esac; }

# Install Codex $1 in place of the current one; sets ROLLBACK.
codex_install() {
  local new=$1 old=$2 path real rel releases root target asset url sum stage newrel l t links=() undo=""
  path=$(command -v codex) || return 1
  real=$(readlink -f "$path")
  case "$real" in
    */node_modules/@openai/codex/*)
      # Global npm install (install.sh on a fresh VM).
      local npmroot; npmroot=$(npm root -g)
      as_owner "$npmroot" npm install -g --no-audit --no-fund --silent "@openai/codex@$new" >/dev/null 2>&1 || { echo "npm install failed"; return 1; }
      ROLLBACK="as_owner '$npmroot' npm install -g --no-audit --no-fund --silent '@openai/codex@$old' >/dev/null 2>&1"
      CLEANUP=:
      return 0 ;;
    */releases/*/bin/codex | */releases/*/codex) ;;
    *) echo "don't know how to update the Codex installed at $real"; return 1 ;;
  esac
  # Standalone layout: …/releases/<release>/bin/codex, reached by symlinks (directly from a bin
  # directory, as in /usr/local, or through a `current` link, as the official installer does).
  rel=${real%/bin/codex}; rel=${rel%/codex}
  releases=$(dirname "$rel"); root=$(dirname "$releases")
  target=$(codex_target) || { echo "unsupported platform $(uname -m)"; return 1; }
  asset="codex-package-$target.tar.gz"
  url="https://github.com/openai/codex/releases/download/rust-v$new"
  sum=$(curl -fsSL --max-time 30 "$url/codex-package_SHA256SUMS" | awk -v a="$asset" '$2 == a {print tolower($1)}')
  [[ "$sum" =~ ^[a-f0-9]{64}$ ]] || { echo "no checksum for $asset"; return 1; }
  curl -fsSL --max-time 600 -o "$TMP/$asset" "$url/$asset" || { echo "download failed"; return 1; }
  [ "$(sha256sum "$TMP/$asset" | cut -d' ' -f1)" = "$sum" ] || { echo "checksum mismatch"; return 1; }
  newrel="$releases/$new-$target"
  stage="$releases/.staging.$new.$$"
  as_owner "$releases" rm -rf "$stage" && as_owner "$releases" mkdir -p "$stage" \
    && as_owner "$releases" tar -xzf "$TMP/$asset" -C "$stage" --no-same-owner || { echo "unpacking failed"; return 1; }
  [ "$("$stage/bin/codex" --version 2>/dev/null | awk 'NR==1{print $NF}')" = "$new" ] || { as_owner "$releases" rm -rf "$stage"; echo "the downloaded binary doesn't run"; return 1; }
  [ -e "$stage/codex" ] || as_owner "$releases" ln -s bin/codex "$stage/codex"
  as_owner "$releases" rm -rf "$newrel" && as_owner "$releases" mv "$stage" "$newrel" || return 1
  if [ -L "$root/current" ] && [ "$(readlink -f "$root/current")" = "$rel" ]; then
    links=("$root/current")
  else
    for l in "$(dirname "$path")"/*; do [ -L "$l" ] && [[ "$(readlink -f "$l")" == "$rel"/* ]] && links+=("$l"); done
  fi
  for l in "${links[@]}"; do
    t=$(readlink "$l")
    if [ "$l" = "$root/current" ]; then relink "$l" "$newrel"; else relink "$l" "$newrel/${t#"$rel"/}"; fi || return 1
    undo+="relink '$l' '$t'; "
    ROLLBACK="$undo"
  done
  [ ${#links[@]} -gt 0 ] || { echo "found no links to $rel to switch"; return 1; }
  CLEANUP="codex_prune '$releases' '$newrel' '$rel'"
}
# Keep only the current and previous release.
codex_prune() { local d; for d in "$1"/*/; do d=${d%/}; [ "$d" = "$2" ] || [ "$d" = "$3" ] || as_owner "$1" rm -rf "$d"; done; }

# ---- one CLI: compare, install, check, keep or roll back ----

update_cli() {
  local engine=$1 have latest why
  wanted "$engine" || return 0
  if ! command -v "$engine" >/dev/null; then record "$engine" "" "not installed"; say "$engine: not installed, skipped"; return 0; fi
  have=$("${engine}_installed")
  latest=$("${engine}_latest")
  if ! is_version "$latest"; then record "$engine" "$have" "latest version unknown"; log "$engine $have: could not find the latest version; skipped"; return 0; fi
  if ! newer "$have" "$latest"; then record "$engine" "$have" "up to date"; say "$engine $have: up to date"; return 0; fi
  if [ -n "$CHECK_ONLY" ]; then record "$engine" "$have" "update available: $latest"; say "$engine $have: $latest available"; return 0; fi
  if ! why=$(ZEN_ENGINES_FAIL='' cli_check "$engine" "$have"); then
    record "$engine" "$have" "skipped: check fails before updating"
    log "$engine $have → $latest skipped: the check already fails on $have ($why)"
    return 0
  fi
  # Runs in this shell (not a subshell) so it can set ROLLBACK and CLEANUP.
  ROLLBACK=: CLEANUP=:
  if ! "${engine}_install" "$latest" "$have" >"$TMP/install.out" 2>&1; then
    # It stopped partway: undo what it had switched already (ROLLBACK grows with each step).
    eval "$ROLLBACK"
    record "$engine" "$("${engine}_installed")" "update failed"
    log "$engine $have → $latest FAILED to install: $(tail -1 "$TMP/install.out"); kept $("${engine}_installed")"
    return 0
  fi
  if why=$(cli_check "$engine" "$latest"); then
    eval "$CLEANUP" || true
    record "$engine" "$latest" "updated from $have"
    log "$engine $have → $latest OK"
  else
    eval "$ROLLBACK"
    rolled_back "$engine" "$("${engine}_installed")" "$have" "$latest" "$why"
  fi
}

# ---- Pi ----

MIND="$REPO/packages/mind"
PI_PKGS=(@earendil-works/pi-ai @earendil-works/pi-agent-core)

# Send the Pi worker one request ($1) and print its first line of output, waiting at most $2 seconds.
# (The worker exits as soon as its stdin closes, so stdin is kept open until it answers.)
pi_rpc() {
  (cd "$MIND" && set -a && { . "$ZEN/env" 2>/dev/null || true; } && set +a && ZEN_FAUX=1 timeout "$2" node -e '
    const c = require("node:child_process").spawn(process.execPath, ["src/main.ts"], { stdio: ["pipe", "pipe", "ignore"] });
    let buf = "";
    c.stdout.on("data", (d) => { buf += d; const i = buf.indexOf("\n"); if (i >= 0) { process.stdout.write(buf.slice(0, i + 1)); c.kill(); process.exit(0); } });
    c.on("exit", () => process.exit(1));
    c.stdin.write(process.argv[1] + "\n");' "$1" 2>/dev/null)
}

pi_check() {
  local want=$1 got req res
  if forced_fail pi; then echo "forced failure (ZEN_ENGINES_FAIL)"; return 1; fi
  got=$(node -p "require('$MIND/node_modules/@earendil-works/pi-ai/package.json').version" 2>/dev/null)
  [ "$got" = "$want" ] || { echo "node_modules has pi-ai ${got:-nothing}, expected $want"; return 1; }
  pi_rpc '{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}' 15 | grep -q pong || { echo "the worker didn't answer ping"; return 1; }
  res=$(pi_rpc '{"jsonrpc":"2.0","id":1,"method":"models.list","params":{}}' 30)
  echo "$res" | jq -e '[.result.models[].id] | index("faux/faux-1")' >/dev/null 2>&1 || { echo "the worker doesn't list its models"; return 1; }
  local s1; s1=$(grep -E '^ZEN_S1_MODEL=' "$ZEN/env" 2>/dev/null | cut -d= -f2-)
  if [ -n "$s1" ] && echo "$res" | jq -e '.result.authenticated.openrouter == true' >/dev/null 2>&1; then
    req=$(jq -nc --arg m "$s1" '{jsonrpc:"2.0",id:1,method:"s1.decide",params:{model:$m,state:{text:"The sky is blue."},questions:{color:{type:"bool",instructions:"Is the text about a color?",criteria:{"true":"yes","false":"no"}}}}}')
    res=$(pi_rpc "$req" 90)
    echo "$res" | jq -e '.result.error == null and .result.answers.color != null' >/dev/null 2>&1 \
      || { echo "a System One call on $s1 failed: $(echo "$res" | jq -r '.result.error // .error.message // "no answer"' 2>/dev/null | head -c 300)"; return 1; }
  fi
}

pi_installed() { node -p "require('$MIND/node_modules/$1/package.json').version" 2>/dev/null || true; }
# Put Pi packages (name@version …) into node_modules only: package.json and the lockfile, which pin
# Pi's minimum version, stay as they are, so the update makes no commit (see scripts/mind-deps.sh).
pi_put() { (cd "$MIND" && npm install --no-save --no-audit --no-fund --silent "$@"); }

update_pi() {
  wanted pi || return 0
  if [ -z "${ZEN_ENGINES:-}" ] && ! pi_enabled; then return 0; fi
  local have p cur latest target="" old=() want=() news why out
  have=$(pi_installed "${PI_PKGS[0]}")
  [ -n "$have" ] || { record pi "" "not installed"; say "pi: not installed (scripts/mind-deps.sh installs it), skipped"; return 0; }
  for p in "${PI_PKGS[@]}"; do
    cur=$(pi_installed "$p")
    latest=$(npm view "$p" version 2>/dev/null)
    is_version "$latest" || { record pi "$have" "latest version unknown"; log "pi $have: could not find the latest version of $p; skipped"; return 0; }
    old+=("$p@$cur")
    if newer "$cur" "$latest"; then want+=("$p@$latest"); fi
    [ "$p" = "${PI_PKGS[0]}" ] && target=$latest
  done
  if [ ${#want[@]} -eq 0 ]; then record pi "$have" "up to date"; say "pi $have: up to date"; return 0; fi
  news=$target; [ "$news" = "$have" ] && news="$have (${want[*]})"
  if [ -n "$CHECK_ONLY" ]; then record pi "$have" "update available: $news"; say "pi $have: $news available"; return 0; fi
  # Applying means a zenbot restart through upgrade.sh, which installs whatever the checkout holds.
  if [ -n "$(git -C "$REPO" status --porcelain --untracked-files=no)" ]; then
    record pi "$have" "skipped: local changes"; log "pi $have → $news skipped: $REPO has local changes"; return 0
  fi
  if [ "$(git -C "$REPO" rev-parse --abbrev-ref HEAD)" != main ]; then
    record pi "$have" "skipped: not on main"; log "pi $have → $news skipped: $REPO is not on main"; return 0
  fi
  if ! out=$(pi_put "${want[@]}" 2>&1); then
    pi_put "${old[@]}" >/dev/null 2>&1; record pi "$(pi_installed "${PI_PKGS[0]}")" "update failed"
    log "pi $have → $news FAILED to install: $(echo "$out" | tail -1); kept $(pi_installed "${PI_PKGS[0]}")"; return 0
  fi
  if ! why=$(pi_check "$target"); then
    pi_put "${old[@]}" >/dev/null 2>&1; rolled_back pi "$(pi_installed "${PI_PKGS[0]}")" "$have" "$news" "$why"; return 0
  fi
  # upgrade.sh runs a scripted Pi turn through a second kernel, then restarts zenbot at idle so the
  # worker loads the new Pi (its scripts/mind-deps.sh keeps the newer version).
  if out=$("$REPO/scripts/upgrade.sh" 2>&1); then
    record pi "$target" "updated from $have"
    log "pi $have → $target OK (node_modules only, no commit; zenbot restarts at idle, see the upgrade lines)"
  else
    pi_put "${old[@]}" >/dev/null 2>&1
    rolled_back pi "$(pi_installed "${PI_PKGS[0]}")" "$have" "$news" "scripts/upgrade.sh failed: $(echo "$out" | grep -E 'FAILED' | head -1)"
  fi
}

update_cli claude
update_cli codex
update_pi

# ---- state for `zen status` ----
if [ -z "$CHECK_ONLY" ]; then
  # Engines not looked at this time (ZEN_ENGINES) keep their last entry.
  json=$(jq --arg at "$(date -u +%FT%TZ)" '{checked: $at, engines: (.engines // {})}' "$STATE" 2>/dev/null || jq -n --arg at "$(date -u +%FT%TZ)" '{checked: $at, engines: {}}')
  for e in "${!VERSION[@]}"; do
    json=$(echo "$json" | jq --arg e "$e" --arg v "${VERSION[$e]}" --arg s "${STATUS[$e]}" '.engines[$e] = {version: $v, status: $s}')
  done
  echo "$json" > "$STATE.tmp" && mv -f "$STATE.tmp" "$STATE"
fi
behind=0
for e in "${!STATUS[@]}"; do [ "${STATUS[$e]}" = "up to date" ] || [[ "${STATUS[$e]}" == updated* ]] || behind=1; done
if [ $behind = 0 ]; then say "engines: all up to date"; else say "engines: not all up to date (see above)"; fi
