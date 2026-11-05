"""Run every Python GUI consumer's smoke test against a vst-audio wheel (Python 3.11+).

Each example loads the native fixtures with the wheel's bundled worker, opens
and closes the worker-owned editor and renders a test tone from its own event
loop (see common.py). The wheel and the toolkits from requirements.txt go into
a fresh venv. On Linux that venv also sees the system site packages, because
Tk, GTK 4 (PyGObject) and wxPython come from the distribution there; run this
script with the system Python under ``xvfb-run`` and ``dbus-run-session``, as
CI does.

``--wheel PATH`` tests an already built wheel; without it a wheel is built
from this checkout first.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "bindings/python"))
from fixtures import prepare

LINUX = sys.platform.startswith("linux")
# PyGObject has no wheels for python.org builds on Windows or macOS; GTK is
# exercised where it is the native toolkit.
TOOLKITS = ["ctk", "pyqt6", "pyside6", "wx"] + (["gtk"] if LINUX else []) + ["webview"]
TIMEOUT = 120


def run(*args, **kwargs):
    subprocess.run(list(map(str, args)), cwd=ROOT, check=True, **kwargs)


def build_wheel(python):
    from stage_worker import stage
    stage()
    wheels = ROOT / "target/python-gui-wheels"
    run(python, "-m", "pip", "install", "--quiet", "maturin==1.16.0")
    run(python, "-m", "maturin", "build", "--locked", "--release", "--interpreter", python,
        "--out", wheels, cwd=ROOT / "bindings/python")
    return max(wheels.glob("vst_audio-*.whl"), key=lambda path: path.stat().st_mtime_ns)


def summary_of(stdout):
    for line in reversed(stdout.splitlines()):
        if line.startswith("{"):
            try:
                return json.loads(line)
            except json.JSONDecodeError:
                return None
    return None


def check(summary, environment):
    """Return why a finished smoke run does not prove the binding worked, or None."""
    if summary is None:
        return "no JSON summary on stdout"
    worker = Path(summary.get("worker", ""))
    problems = [
        summary.get("exit") != 0 and f"summary exit {summary.get('exit')}: {summary.get('error')}",
        (summary.get("inputs"), summary.get("outputs")) != (2, 6)
        and f"fixture reported {summary.get('inputs')} in, {summary.get('outputs')} out",
        not summary.get("busy_heartbeats") and "the GUI timer never ran while a call was in flight",
        not (len(summary.get("peaks", [])) == 6 and min(summary["peaks"]) > 0)
        and f"render peaks {summary.get('peaks')}",
        # The point of the package: its own worker, not one from the build tree.
        not worker.resolve().is_relative_to(environment.resolve())
        and f"worker {worker} is not the wheel's bundled worker",
    ]
    return "; ".join(problem for problem in problems if problem) or None


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--wheel", type=Path, help="test this wheel instead of building one")
    parser.add_argument("--python", default=sys.executable,
                        help="base interpreter for the venv (default: this one)")
    parser.add_argument("--toolkit", action="append", choices=TOOLKITS,
                        help="run only this toolkit (repeatable)")
    args = parser.parse_args()
    _, vst2, vst3 = prepare()

    environment = ROOT / "target/python-gui-test-venv"
    venv = [args.python, "-m", "venv", "--clear"] + (["--system-site-packages"] if LINUX else [])
    run(*venv, environment)
    python = environment / ("Scripts/python.exe" if sys.platform == "win32" else "bin/python")
    run(python, "-m", "pip", "install", "--quiet", "--upgrade", "pip")
    wheel = args.wheel.resolve() if args.wheel else build_wheel(python)
    run(python, "-m", "pip", "install", "--quiet", wheel, "-r", HERE / "requirements.txt")
    run(python, "-m", "pip", "list")

    env = dict(os.environ)
    # The bundled worker and the installed package only.
    for name in ("VST_AUDIO_WORKER", "PYTHONPATH"):
        env.pop(name, None)
    if LINUX:
        env.setdefault("PYWEBVIEW_GUI", "gtk")
        # Xvfb has no GPU: GTK 4 and WebKitGTK draw in software.
        env.setdefault("GSK_RENDERER", "cairo")
        env.setdefault("LIBGL_ALWAYS_SOFTWARE", "1")
        env.setdefault("WEBKIT_DISABLE_DMABUF_RENDERER", "1")
    output = ROOT / "test-results"
    output.mkdir(exist_ok=True)

    failures = []
    for toolkit in args.toolkit or TOOLKITS:
        for name, plugin in (("vst2", vst2), ("vst3", vst3)):
            command = [str(python), str(HERE / f"{toolkit}_host.py"),
                       "--plugin", str(plugin), "--smoke"]
            try:
                result = subprocess.run(command, cwd=ROOT, env=env, capture_output=True,
                                        text=True, encoding="utf-8", errors="replace",
                                        timeout=TIMEOUT)
                code, stdout, stderr = result.returncode, result.stdout, result.stderr
            except subprocess.TimeoutExpired as hung:
                # Keep what the hung GUI printed; it is the only evidence.
                def text(value):
                    return value.decode("utf-8", "replace") if isinstance(value, bytes) else value or ""
                code, stdout, stderr = "timeout", text(hung.stdout), text(hung.stderr)
            (output / f"python-{toolkit}-{name}.log").write_text(stdout + stderr, encoding="utf-8")
            problem = f"exit {code}" if code != 0 else check(summary_of(stdout), environment)
            print(f"python {toolkit} / {name.upper()}: {problem or 'passed'}", flush=True)
            if problem:
                print(stderr[-4000:], flush=True)
                failures.append(f"{toolkit}/{name}")
    if failures:
        raise SystemExit(f"failed: {', '.join(failures)}")


if __name__ == "__main__":
    main()
