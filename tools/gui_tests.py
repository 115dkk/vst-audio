"""Exercise two GUI event loops against the public IPC API, with real windows."""
import os
from pathlib import Path
import subprocess
import sys
import time
import shutil
from fixtures import prepare

ROOT = Path(__file__).resolve().parents[1]
worker, vst2, vst3 = prepare()
subprocess.run(["cargo", "build", "--locked", "--manifest-path",
                "examples/gui/Cargo.toml", "--workspace"], cwd=ROOT, check=True)
gui_target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "examples/gui/target")) / "debug"
extension = ".exe" if sys.platform == "win32" else ""
fixtures = [vst2, vst3]
output = ROOT / "test-results"
output.mkdir(exist_ok=True)
for gui in ["egui", "iced"]:
    for index, plugin in enumerate(fixtures):
        command = [str(gui_target / f"vst-audio-{gui}-example{extension}"),
                   "--worker", str(worker), "--plugin", str(plugin), "--smoke"]
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=35, env=dict(os.environ,VST_AUDIO_TRACE="1",RUST_BACKTRACE="1"))
        (output / f"{gui}-vst{2 + index}.log").write_text(
            result.stdout + result.stderr, encoding="utf-8")
        print(f"{gui} / VST{2 + index}: exit {result.returncode}", flush=True)
        if result.returncode:
            if sys.platform=="darwin":
                for report in (Path.home()/"Library/Logs/DiagnosticReports").glob("vst-audio-worker*"):
                    if report.stat().st_mtime > time.time()-180: shutil.copyfile(report,output/report.name)
            print(result.stderr)
            raise SystemExit(result.returncode)
