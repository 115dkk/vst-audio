"""Exercise two GUI event loops against the public IPC API, with real windows."""
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
subprocess.run(["cargo", "build", "--locked", "-p", "vst-audio-worker",
                "-p", "vst-audio-testplugin"], cwd=ROOT, check=True)
subprocess.run(["cargo", "build", "--locked", "--manifest-path",
                "examples/gui/Cargo.toml", "--workspace"], cwd=ROOT, check=True)
target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "debug"
gui_target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "examples/gui/target")) / "debug"
extension = ".exe" if sys.platform == "win32" else ""
library = {"win32": "vst_audio_testplugin.dll", "darwin": "libvst_audio_testplugin.dylib"}.get(sys.platform, "libvst_audio_testplugin.so")
worker = target / ("vst-audio-worker" + extension)
fixtures = [target / library, target / "vst_audio_testplugin.vst3"]
# The VST3 filename selects its format; do not require proprietary fixtures.
import shutil
shutil.copyfile(fixtures[0], fixtures[1])
output = ROOT / "test-results"
output.mkdir(exist_ok=True)
for gui in ["egui", "iced"]:
    for index, plugin in enumerate(fixtures):
        command = [str(gui_target / f"vst-audio-{gui}-example{extension}"),
                   "--worker", str(worker), "--plugin", str(plugin), "--smoke"]
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=35)
        (output / f"{gui}-vst{2 + index}.log").write_text(
            result.stdout + result.stderr, encoding="utf-8")
        print(f"{gui} / VST{2 + index}: exit {result.returncode}", flush=True)
        if result.returncode:
            print(result.stderr)
            raise SystemExit(result.returncode)
