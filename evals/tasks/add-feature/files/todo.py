#!/usr/bin/env python3
"""A tiny todo list kept in todo.txt, one item per line."""
import sys

FILE = "todo.txt"


def load():
    try:
        with open(FILE) as f:
            return [line.rstrip("\n") for line in f if line.strip()]
    except FileNotFoundError:
        return []


def save(items):
    with open(FILE, "w") as f:
        f.writelines(item + "\n" for item in items)


def main(args):
    if not args or args[0] == "list":
        for i, item in enumerate(load(), 1):
            print(f"{i}. {item}")
    elif args[0] == "add":
        items = load()
        items.append(" ".join(args[1:]))
        save(items)
    else:
        print(f"unknown command: {args[0]}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
