#!/usr/bin/env bash
# The web app (web/): build it from this checkout, and install a build as the user service
# `zen-web` on 127.0.0.1:4400 (ZEN_WEB_PORT), where a signing-in proxy in front sends the traffic.
# upgrade.sh builds it with the kernel and apply-upgrade.sh installs both, so they can't drift.
#
#   scripts/web-install.sh build <dir>            the checkout's web/ (uncommitted changes too), built
#   scripts/web-install.sh install <dir> <name>   run that build as ~/.local/share/zen-web/<name>
#
# install keeps the build that ran before it and goes back to it if the new one doesn't answer.
# Settings for the service go in ~/.config/zen-web.env (ZEN_WEB_* in web/README.md); kernel
# secrets in ~/.zenbot/env never reach it. It replaces the older `zenwrite` user unit.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="$HOME/.local/node/bin:$PATH"
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
export DBUS_SESSION_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}
# ZEN_WEB_UNIT names another unit, with its own builds and settings (for trying this script out).
NAME=${ZEN_WEB_UNIT:-zen-web}
APPS=$HOME/.local/share/$NAME
UNIT=$NAME.service
SETTINGS=$HOME/.config/$NAME.env

setting() { { grep -E "^$1=" "$SETTINGS" 2>/dev/null || true; } | tail -1 | cut -d= -f2-; }

build() { # out
  local out=$1
  command -v node >/dev/null && node --version | grep -qE '^v(2[2-9]|[3-9][0-9])\.' \
    || { echo "web: Node 22 or newer is needed (install.sh puts it in ~/.local/node)" >&2; return 1; }
  rm -rf "$out" && mkdir -p "$out"
  # The files git would see: tracked and new ones, not ignored ones (node_modules, dist).
  (cd "$REPO/web" && git ls-files -z -co --exclude-standard . | xargs -0 -r ls -d 2>/dev/null | tar -cf - -T -) | tar -xf - -C "$out"
  # Its packages only for the build: the server runs on Node alone.
  (cd "$out" && npm ci --no-audit --no-fund --loglevel=error && npm run build --silent && rm -rf node_modules)
  [ -f "$out/dist/index.html" ] || { echo "web: the build made no dist/index.html" >&2; return 1; }
}

answers() { # port
  local h; h=$(setting ZEN_WEB_AUTH_HEADER); h=${h:-X-ExeDev-UserID}
  curl -sf -o /dev/null --max-time 5 -H "$h: install-check" "http://127.0.0.1:$1/"
}

install_build() { # dir name
  local src=$1 name=$2 node port dir previous
  node=$(command -v node) || { echo "web: no node on PATH" >&2; return 1; }
  [ -f "$src/server/main.ts" ] && [ -f "$src/dist/index.html" ] || { echo "web: $src is not a build" >&2; return 1; }
  port=$(setting ZEN_WEB_PORT); port=${port:-4400}
  mkdir -p "$APPS" "$HOME/.config/systemd/user"
  dir=$APPS/$name
  rm -rf "$dir" && mv "$src" "$dir"

  cat > "$HOME/.config/systemd/user/$UNIT" <<UNIT
[Unit]
Description=zen web app on 127.0.0.1:$port

[Service]
WorkingDirectory=$APPS/current
ExecStart=$node server/main.ts
Restart=on-failure
Environment=ZEN_WEB_PORT=$port
EnvironmentFile=-$SETTINGS

[Install]
WantedBy=default.target
UNIT

  # The older unit held the same port; it goes once, here (and comes back if this build fails
  # with no build of ours to go back to).
  local retired=
  if [ "$NAME" = zen-web ] && systemctl --user is-enabled --quiet zenwrite.service 2>/dev/null; then
    systemctl --user disable --now --quiet zenwrite.service && retired=1
  fi
  previous=$(readlink "$APPS/current" 2>/dev/null || true)
  [ "$previous" != "$dir" ] || previous=
  ln -sfn "$dir" "$APPS/current"
  systemctl --user daemon-reload
  systemctl --user enable --quiet "$UNIT"
  systemctl --user restart "$UNIT"

  local i
  for i in $(seq 30); do answers "$port" && break; sleep 0.5; done
  if ! answers "$port"; then
    echo "web: $name did not answer on :$port; rolling back${previous:+ to $(basename "$previous")}" >&2
    journalctl --user -u "$UNIT" -n 12 --no-pager -o cat >&2 2>/dev/null || true
    if [ -n "$previous" ] && [ -d "$previous" ]; then
      ln -sfn "$previous" "$APPS/current"
      systemctl --user reset-failed "$UNIT" 2>/dev/null || true # a crash loop hits the start limit
      systemctl --user restart "$UNIT"
      for i in $(seq 30); do answers "$port" && break; sleep 0.5; done
      answers "$port" && rm -rf "$dir" || echo "web: the previous build doesn't answer either" >&2
    elif [ -n "$retired" ]; then
      systemctl --user disable --now --quiet "$UNIT" || true
      systemctl --user enable --now --quiet zenwrite.service && echo "web: the older zenwrite unit is back" >&2
    fi
    return 1
  fi
  # Keep this build and the one before it.
  local old
  for old in "$APPS"/*/; do
    old=${old%/}
    [ ! -L "$old" ] || continue
    [ "$old" = "$dir" ] || [ "$old" = "$previous" ] || rm -rf "$old"
  done
  echo "web: $name running on :$port"
}

case ${1:-} in
  build) build "${2:?usage: $0 build <dir>}" ;;
  install) install_build "${2:?usage: $0 install <dir> <name>}" "${3:?usage: $0 install <dir> <name>}" ;;
  *) echo "usage: $0 build <dir> | install <dir> <name>" >&2; exit 2 ;;
esac
