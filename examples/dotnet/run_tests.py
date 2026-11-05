"""Build the WPF and WinUI 3 consumers against a VstAudio package and run their smoke tests.

Windows only (Python 3.11+). Each app loads the native fixtures with the
package's bundled worker, opens and closes the worker-owned editor and renders
a test tone while its UI thread keeps running, then writes a JSON summary.

``--package-dir DIR`` uses an existing packed VstAudio 0.1.0, such as the
multi-platform package CI builds; without it the package is packed from this
checkout for the current platform first.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import uuid

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "bindings/python"))
sys.path.insert(0, str(ROOT / "bindings/dotnet"))
from fixtures import prepare

APPS = {"WpfHost": "wpf", "WinUIHost": "winui"}
TIMEOUT = 120


def run(*args):
    subprocess.run(list(map(str, args)), cwd=ROOT, check=True)


def check(summary, toolkit, output):
    """Return why a finished smoke run does not prove the package worked, or None."""
    worker = Path(summary.get("worker", ""))
    problems = [
        summary.get("exit") != 0 and f"summary exit {summary.get('exit')}: {summary.get('error')}",
        summary.get("toolkit") != toolkit and f"toolkit {summary.get('toolkit')!r}",
        (summary.get("inputs"), summary.get("outputs")) != (2, 6)
        and f"fixture reported {summary.get('inputs')} in, {summary.get('outputs')} out",
        not summary.get("busyHeartbeats") and "the UI timer never ran while a call was in flight",
        not (len(summary.get("peaks", [])) == 6 and min(summary["peaks"]) > 0)
        and f"render peaks {summary.get('peaks')}",
        # The package's own worker from the app's output, not one from the build tree.
        not worker.resolve().is_relative_to(output.resolve())
        and f"worker {worker} is not the package's bundled worker",
    ]
    return "; ".join(problem for problem in problems if problem) or None


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--package-dir", type=Path, help="directory holding VstAudio.0.1.0.nupkg")
    parser.add_argument("--app", action="append", choices=list(APPS), help="run only this app")
    args = parser.parse_args()
    if sys.platform != "win32":
        raise SystemExit("WPF and WinUI 3 run only on Windows")
    _, vst2, vst3 = prepare()
    package_dir = args.package_dir
    if package_dir is None:
        from stage_native import stage_native
        stage_native()
        package_dir = ROOT / "target/dotnet-pack"
        run("dotnet", "pack", ROOT / "bindings/dotnet/VstAudio/VstAudio.csproj",
            "--configuration", "Release", "--output", package_dir)
    package_dir = package_dir.resolve()
    if not (package_dir / "VstAudio.0.1.0.nupkg").is_file():
        raise FileNotFoundError(f"VstAudio.0.1.0.nupkg missing in {package_dir}")

    env = dict(os.environ)
    env.pop("VST_AUDIO_WORKER", None)
    results = ROOT / "test-results"
    results.mkdir(exist_ok=True)
    failures = []
    for app, toolkit in APPS.items():
        if args.app and app not in args.app:
            continue
        output = ROOT / "target/dotnet-gui" / app
        # A unique packages path forces restore to read this exact package.
        packages = ROOT / "target/dotnet-packages" / uuid.uuid4().hex
        run("dotnet", "build", HERE / app / f"{app}.csproj", "--configuration", "Release",
            f"-p:VstAudioPackageDir={package_dir}", f"-p:RestorePackagesPath={packages}",
            "--output", output)
        # VstAudio 0.1.0 is on nuget.org too; make sure restore took the package under test.
        metadata = json.loads((packages / "vstaudio/0.1.0/.nupkg.metadata").read_text(encoding="utf-8"))
        if Path(metadata["source"]).resolve() != package_dir:
            raise SystemExit(f"{app} restored VstAudio from {metadata['source']}, not {package_dir}")
        for name, plugin in (("vst2", vst2), ("vst3", vst3)):
            summary_path = results / f"dotnet-{toolkit}-{name}.json"
            summary_path.unlink(missing_ok=True)
            command = [str(output / f"{app}.exe"), "--plugin", str(plugin), "--smoke",
                       "--summary", str(summary_path)]
            try:
                result = subprocess.run(command, cwd=ROOT, env=env, capture_output=True,
                                        text=True, encoding="utf-8", errors="replace",
                                        timeout=TIMEOUT)
                code, log = result.returncode, result.stdout + result.stderr
            except subprocess.TimeoutExpired as hung:
                def text(value):
                    return value.decode("utf-8", "replace") if isinstance(value, bytes) else value or ""
                code, log = "timeout", text(hung.stdout) + text(hung.stderr)
            (results / f"dotnet-{toolkit}-{name}.log").write_text(log, encoding="utf-8")
            if code != 0:
                problem = f"exit {code}"
            elif not summary_path.is_file():
                problem = "no summary file"
            else:
                problem = check(json.loads(summary_path.read_text(encoding="utf-8")), toolkit, output)
            print(f"dotnet {toolkit} / {name.upper()}: {problem or 'passed'}", flush=True)
            if problem:
                print(log[-4000:], flush=True)
                failures.append(f"{toolkit}/{name}")
    if failures:
        raise SystemExit(f"failed: {', '.join(failures)}")


if __name__ == "__main__":
    main()
