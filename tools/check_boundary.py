"""The safe application dependency graph must not load foreign plugins or a GUI."""
import json
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[1]
metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--format-version", "1"], cwd=root))
packages = {p["id"]: p for p in metadata["packages"]}
nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
start = next(p["id"] for p in packages.values() if p["name"] == "vst-audio")
pending, seen = [start], set()
while pending:
    key = pending.pop()
    if key in seen:
        continue
    seen.add(key)
    for dep in nodes[key]["deps"]:
        if any(k["kind"] != "dev" for k in dep["dep_kinds"]):
            pending.append(dep["pkg"])
names = {packages[key]["name"] for key in seen}
forbidden = {n for n in names if n.startswith("vst-audio-worker") or n in {
    "tauri", "egui", "iced", "slint", "gtk4", "cpal", "libloading"}}
if forbidden:
    raise SystemExit(f"application boundary violated: {sorted(forbidden)}")
print("Application dependency boundary passed: no worker, loader, GUI or audio device backend.")
