"""Warehouse inventory."""

ITEMS = [("pallets", 12), ("crates", 40), ("drums", 3)]


def total(items):
    return sum(count for _, count in items)
