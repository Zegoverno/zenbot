#!/usr/bin/env bash
# Source after cargo build. Python's system SQLite on Ubuntu 22.04 predates JSON operators and
# ordered aggregates. Build a test-only shared library from Cargo's checksum-verified, pinned
# libsqlite3-sys amalgamation when needed; release binaries keep the Ubuntu 22.04 glibc baseline.
ensure_test_sqlite() {
  python3 -c 'import sqlite3,sys;sys.exit(sqlite3.sqlite_version_info < (3,44,0))' && return 0
  local test_repo version source candidate destination
  test_repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  version=$(python3 - "$test_repo/Cargo.lock" <<'PY'
import pathlib,re,sys
text=pathlib.Path(sys.argv[1]).read_text()
match=re.search(r'name = "libsqlite3-sys"\nversion = "([^"]+)"',text)
if not match:
    raise SystemExit('Cargo.lock has no pinned libsqlite3-sys')
print(match[1])
PY
  ) || return
  source=
  for candidate in "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/libsqlite3-sys-"$version"/sqlite3/sqlite3.c; do
    [ ! -f "$candidate" ] || { source=$candidate; break; }
  done
  if [ -z "$source" ]; then
    echo 'Python SQLite >=3.44 required for test assertions; run cargo build to fetch the pinned SQLite source first.' >&2
    return 1
  fi
  destination="$test_repo/target/test-sqlite-$version"
  mkdir -p "$destination"
  if [ ! -s "$destination/libsqlite3.so.0" ]; then
    echo "Building pinned SQLite $version for Python test helpers" >&2
    cc -O2 -fPIC -shared -DSQLITE_THREADSAFE=1 -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_COLUMN_METADATA \
      "$source" -o "$destination/libsqlite3.so.0.tmp.$$" -ldl -lpthread -lm || return
    mv "$destination/libsqlite3.so.0.tmp.$$" "$destination/libsqlite3.so.0"
  fi
  export LD_LIBRARY_PATH="$destination${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  python3 -c 'import sqlite3,sys;sys.exit(sqlite3.sqlite_version_info < (3,44,0))'
}
