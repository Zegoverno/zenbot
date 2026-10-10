#!/usr/bin/env python3
"""Checks that the docs still match the code: the mechanical half of keeping them true.

Each check pairs a list the code defines (routes, tools, settings, tables, events, tape kinds,
slash commands, e2e scenarios, crates) with the doc whose job is to list it, and fails when one
has an item the other lacks. It also checks that repo paths and decision numbers named in the
docs exist. It can't tell whether a sentence is still true: that stays the job of whoever changes
the code (AGENTS.md, "Update the docs before shipping").

  scripts/check-docs.py                    # the checks above
  scripts/check-docs.py --base origin/main # also: a change to the code must add to PROGRESS.md

Test-only settings (ZEN_TEST_*) are exempt. History is exempt from the "current docs" checks: PROGRESS.md, DECISIONS.md and docs/research/
describe what was true when they were written. CI runs this on every pull request.
"""
import glob
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
os.chdir(ROOT)

HISTORY = ("PROGRESS.md", "DECISIONS.md", "docs/research/")
problems = []


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def tracked(*patterns):
    out = subprocess.run(["git", "ls-files", *patterns], capture_output=True, text=True, check=True)
    return [p for p in out.stdout.splitlines() if os.path.exists(p)]


def code(globs):
    """The concatenated text of the tracked files matching the globs (tests included)."""
    return "\n".join(read(p) for p in tracked(*globs))


def current_docs():
    return [p for p in tracked("*.md")
            if not p.startswith(HISTORY) and not p.startswith("evals/tasks/")]


def require(kind, items, doc, text=None, fmt="`{}`", whole=False):
    """Every item must appear in the doc (as fmt, `x` by default; whole: not as part of a longer name)."""
    text = read(doc) if text is None else text
    for item in sorted(items):
        needle = fmt.format(item)
        found = re.search(re.escape(needle) + r"(?![\w/{-])", text) if whole else needle in text
        if not found:
            problems.append(f"{doc}: {kind} {item!r} is in the code but not in this doc")


# --- Lists the code defines, and the doc that lists them -------------------------------------------

kernel = code(["crates/zend/src/*.rs"])

# Routes: every HTTP route, in MAP.md and in the client protocol.
routes = set(re.findall(r'\.route\("([^"]+)"', kernel))
api_routes = {r if r in ("/", "/health") else "/api" + r for r in routes}
for doc in ("MAP.md", "docs/client-protocol.md"):
    require("route", api_routes - {"/"}, doc, fmt="{}", whole=True)

# Tools: every tool the kernel offers, in DESIGN.md's and MAP.md's lists.
tools = set(re.findall(r'^\s*"name": "([a-z_]+)",\s*$', kernel, re.M))
tools |= set(re.findall(r'\bspec\(\s*"([a-z_]+)"', kernel))
for doc in ("DESIGN.md", "MAP.md"):
    require("tool", tools, doc)

# Settings: every ZEN_* / MATRIX_* variable the code or scripts read is documented in MAP.md, and
# every one a current doc names still exists (a trailing _ or * marks a family: ZEN_FAILOVER_*).
code_text = code(["crates/**/*.rs", "scripts/*", "scripts/**/*", "install.sh", "deploy/*"])
env_re = r"\b((?:ZEN|MATRIX)_[A-Z0-9_]*[A-Z0-9])\b"
code_vars = {v for v in re.findall(env_re, code_text) if not v.startswith("ZEN_TEST_")}
require("setting", code_vars, "MAP.md", fmt="{}", whole=True)
for doc in current_docs():
    for var in set(re.findall(env_re + r"(_?\*)?", read(doc))):
        name, family = var
        if family:
            if not any(v.startswith(name) for v in code_vars):
                problems.append(f"{doc}: setting family {name}* matches nothing in the code")
        elif name not in code_vars:
            problems.append(f"{doc}: setting {name} doesn't exist in the code or scripts")

# Tables: every table a migration creates, in MAP.md.
tables = set(re.findall(r"CREATE TABLE (?:IF NOT EXISTS )?(\w+)", code(["crates/zend/migrations/*.sql"]), re.I))
require("table", tables, "MAP.md")

# WebSocket events: every event type the kernel sends, in the client protocol.
events = set()
for m in re.finditer(r"emit\(", kernel):
    events |= set(re.findall(r'"type": "([a-z_]+)"', kernel[m.end():m.end() + 300].split(".await")[0]))
events |= set(re.findall(r'json!\(\{ "type": "(resync)"', kernel))
require("event", events, "docs/client-protocol.md")

# Tape kinds: every kind of block the kernel appends, in docs/context.md.
kinds = set(re.findall(r'tape::append\([^;]*?"([a-z_]+)"', kernel, re.S))
require("tape kind", kinds, "docs/context.md")

# Slash commands: every command zen offers, in the README.
tui = code(["crates/zen/src/**/*.rs", "crates/zen/src/*.rs"])
require("slash command", set(re.findall(r'cmd\("(/[a-z-]+)"', tui)), "README.md", fmt="{}", whole=True)

# End-to-end scenarios: every scenario e2e.sh runs, in MAP.md's list.
require("e2e scenario", set(re.findall(r"^run ([a-z0-9-]+) ", read("scripts/e2e.sh"), re.M)), "MAP.md")

# Crates: every crate, in AGENTS.md's "What runs where" and in MAP.md.
crates = {os.path.basename(p) for p in glob.glob("crates/*") if os.path.isdir(p)}
for doc in ("AGENTS.md", "MAP.md"):
    require("crate", {f"crates/{c}" for c in crates}, doc)

# Docs: every root doc and docs/*.md is in AGENTS.md's table of what to update when.
agents = read("AGENTS.md")
for doc in tracked("*.md", "docs/*.md"):
    if "/" in doc and not doc.startswith("docs/") or doc.startswith("docs/research/") or doc == "AGENTS.md":
        continue
    if f"`{doc}`" not in agents and not (doc.startswith("docs/") and "`docs/*.md`" in agents):
        problems.append(f"AGENTS.md: {doc} is missing from the table of docs to update")

# --- Names the docs use must exist ------------------------------------------------------------------

# SPEC.md names target paths that aren't built yet, so it is exempt from this one.
paths_re = re.compile(r"`((?:crates|scripts|docs|deploy|evals|\.github)/[A-Za-z0-9_./*{}-]+?)(?::\d[\d,-]*)?`")
for doc in (d for d in current_docs() if d != "SPEC.md"):
    for path in set(paths_re.findall(read(doc))):
        path = path.rstrip("/.")
        if any(c in path for c in "*{"):
            if not glob.glob(path.replace("{", "*").replace("}", ""), recursive=True):
                problems.append(f"{doc}: {path} matches no file")
        elif not os.path.exists(path):
            problems.append(f"{doc}: {path} doesn't exist")

decisions = set(re.findall(r"^## (D-\d{3})\b", read("DECISIONS.md"), re.M))
for doc in tracked("*.md"):
    for ref in set(re.findall(r"\bD-\d{3}\b", read(doc))):
        if ref not in decisions:
            problems.append(f"{doc}: {ref} isn't in DECISIONS.md")

# --- A change to the code says what shipped ---------------------------------------------------------

if "--base" in sys.argv:
    base = sys.argv[sys.argv.index("--base") + 1]
    diff = subprocess.run(["git", "diff", "--name-only", f"{base}...HEAD"], capture_output=True, text=True, check=True)
    changed = diff.stdout.split()
    shipped = [p for p in changed if p.startswith(("crates/", "scripts/", "deploy/", "install.sh"))]
    log = subprocess.run(["git", "log", "--format=%B", f"{base}..HEAD"], capture_output=True, text=True, check=True)
    if shipped and "PROGRESS.md" not in changed and "[no-progress]" not in log.stdout:
        problems.append("PROGRESS.md: this change touches the code but adds no entry "
                        "(write one, or put [no-progress] in a commit message if nothing shipped)")

if problems:
    print("Docs out of step with the code:")
    for p in problems:
        print("  " + p)
    print(f"{len(problems)} problem(s). Fix the doc (or the check, if it's wrong), see AGENTS.md.")
    sys.exit(1)
print("docs: ok")
