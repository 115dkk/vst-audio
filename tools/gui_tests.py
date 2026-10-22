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
# GUI_TEST_ROUNDS repeats the matrix; a diagnosis job uses it to catch
# intermittent failures. Every round must pass.
rounds = int(os.environ.get("GUI_TEST_ROUNDS", "1"))
started = time.time()


def crash_reports():
    """Copy every macOS crash report written since this script started."""
    for folder in (Path.home() / "Library/Logs/DiagnosticReports",
                   Path("/Library/Logs/DiagnosticReports")):
        for report in folder.glob("*") if folder.is_dir() else []:
            try:
                if report.is_file() and report.stat().st_mtime >= started - 5:
                    shutil.copyfile(report, output / report.name)
            except OSError:
                pass


for round_ in range(1, rounds + 1):
    for gui in ["egui", "iced"]:
        for index, plugin in enumerate(fixtures):
            command = [str(gui_target / f"vst-audio-{gui}-example{extension}"),
                       "--worker", str(worker), "--plugin", str(plugin), "--smoke"]
            result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=35, env=dict(os.environ,VST_AUDIO_TRACE="1",RUST_BACKTRACE="1"))
            (output / f"{gui}-vst{2 + index}.log").write_text(
                result.stdout + result.stderr, encoding="utf-8")
            print(f"round {round_} {gui} / VST{2 + index}: exit {result.returncode}", flush=True)
            if result.returncode:
                print(result.stderr)
                if sys.platform == "darwin":
                    time.sleep(10)  # ReportCrash writes asynchronously.
                    crash_reports()
                raise SystemExit(result.returncode)
