import json
with open("tasks.json") as f:
    tasks = json.load(f)
print(f"{sum(1 for t in tasks if not t['done'])} open tasks")
