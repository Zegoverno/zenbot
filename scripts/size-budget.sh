#!/usr/bin/env bash
# Fails when a binary outgrows its size budget (D-061): the size measured on 2026-10-14 plus about
# 15%, per build profile. A dependency or a feature that pushes past it is a choice to make on
# purpose: raise the number here and say why in the commit.
#
#   scripts/size-budget.sh                  target/release (what scripts/check.sh and CI build)
#   scripts/size-budget.sh target/dist      the binaries CI publishes (Cargo.toml, profile `dist`)
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR=${1:-$REPO/target/release}
case "$(basename "$DIR")" in
  dist) BUDGET=(zend 17400 zen 5800 zen-engine 1800) ;; # KiB; measured 12871, 4894, 1519
  *) BUDGET=(zend 22400 zen 7900 zen-engine 2500) ;;    # KiB; measured 16928, 6736, 2109
esac
# D-068 adds durable queues/exports, MCP containment and a vendored public suffix list.
# Raise only zend's budget for the combined feature set; the client/worker budgets stay fixed.
# D-072/D-073 (MCP sign-in, script jobs, the learning router): zend +1000 KiB release, +800 dist.
over=0
for ((i = 0; i < ${#BUDGET[@]}; i += 2)); do
  name=${BUDGET[i]} max=${BUDGET[i + 1]}
  kib=$(( $(stat -c %s "$DIR/$name") / 1024 ))
  if [ "$kib" -gt "$max" ]; then status="OVER"; over=1; else status="ok"; fi
  printf '%-11s %6s KiB  budget %6s KiB  %s\n' "$name" "$kib" "$max" "$status"
done
[ "$over" = 0 ] || { echo "a binary is over its size budget (scripts/size-budget.sh)" >&2; exit 1; }
