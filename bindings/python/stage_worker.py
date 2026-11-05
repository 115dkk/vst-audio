"""Stage a native worker and redistribution notices for a Python wheel (Python 3.11+)."""
import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = Path(__file__).resolve().parent / "python/vst_audio"
sys.path.insert(0, str(ROOT / "tools"))
from fixtures import copy_fresh, target_directory
from package_worker import dependency_notices


def run(*args):
    subprocess.run(list(map(str, args)), cwd=ROOT, check=True)


def cargo_target():
    return os.environ.get("CARGO_BUILD_TARGET") or re.search(
        r"^host: (.+)$", subprocess.check_output(["rustc", "-vV"], cwd=ROOT, text=True), re.M
    ).group(1)


def stage_notices(destination, manifest=None, root="vst-audio-worker", inventory="dependencies.json"):
    """Add the licence texts of `root`'s dependency graph to destination/licenses."""
    import tempfile
    # The shared notice collector creates fresh package directories. Generate in
    # a new local staging directory, then merge into the notice tree; packages
    # shared with an earlier graph carry the same texts.
    with tempfile.TemporaryDirectory(prefix="notices-", dir=destination) as temporary:
        scratch = Path(temporary)
        notices = dependency_notices(scratch, cargo_target(), manifest, root)
        shutil.copytree(scratch, destination / "licenses", dirs_exist_ok=True)
    (destination / inventory).write_text(json.dumps(notices, indent=2), encoding="utf-8")


def stage(worker=None, destination=None):
    """Build or copy the native worker into destination (the Python package by default).

    Staging into the Python package also adds the notices of the extension
    module's own Rust dependencies, which it links statically.
    """
    extension = destination is None
    if worker is None:
        run("cargo", "build", "--locked", "--release", "-p", "vst-audio-worker")
        worker = target_directory() / "release" / (
            "vst-audio-worker.exe" if sys.platform == "win32" else "vst-audio-worker"
        )
    worker = Path(worker).resolve()
    destination = Path(destination).resolve() if destination is not None else PACKAGE / "_worker"
    destination.mkdir(parents=True, exist_ok=True)
    if sys.platform == "darwin":
        app = destination / "VstAudioWorker.app"
        executable = app / "Contents/MacOS/vst-audio-worker"
        if worker.is_dir():
            shutil.copytree(worker, app, dirs_exist_ok=True)
        else:
            executable.parent.mkdir(parents=True, exist_ok=True)
            copy_fresh(worker, executable)
            version = re.search(r'^version = "([^"]+)"',
                                (ROOT / "Cargo.toml").read_text(), re.M).group(1)
            info = {
                "CFBundleExecutable": "vst-audio-worker",
                "CFBundleIdentifier": "io.github.115dkk.vst-audio.worker",
                "CFBundleName": "VST Audio Worker", "CFBundlePackageType": "APPL",
                "CFBundleShortVersionString": version, "CFBundleVersion": version,
                "LSUIElement": True,
            }
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
        executable.chmod(0o755)
        entitlements = destination / "worker.entitlements.plist"
        entitlements.write_bytes(plistlib.dumps({
            "com.apple.security.cs.disable-library-validation": True,
        }))
        run("codesign", "--force", "--sign", "-", "--options", "runtime",
            "--entitlements", entitlements, app)
        run("codesign", "--verify", "--strict", app)
    else:
        executable = destination / (
            "vst-audio-worker.exe" if sys.platform == "win32" else "vst-audio-worker"
        )
        copy_fresh(worker, executable)
        if sys.platform != "win32":
            executable.chmod(0o755)
    for document in ("LICENSE-MIT", "LICENSE-APACHE", "NOTICE", "LICENSE-ISC-DPF"):
        shutil.copyfile(ROOT / document, destination / document)
    header = (ROOT / "crates/vst-audio-worker-sys/src/vst2.rs").read_text().split("\n\n#!", 1)[0]
    (destination / "LICENSE-BSD-3-Clause-VST2.txt").write_text(
        "\n".join(line.removeprefix("// ") for line in header.splitlines()), encoding="utf-8"
    )
    stage_notices(destination)
    if extension:
        stage_notices(destination, Path(__file__).resolve().parent / "Cargo.toml",
                      "vst-audio-python", "dependencies-extension.json")
    return executable


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", type=Path, help="Use an existing worker executable or macOS app bundle")
    parser.add_argument("--destination", type=Path, help="Stage into this directory instead of the Python package")
    args = parser.parse_args()
    print(stage(args.worker, args.destination))


if __name__ == "__main__":
    main()
