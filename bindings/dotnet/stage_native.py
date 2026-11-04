"""Stage the C ABI library and matching native worker for NuGet (Python 3.11+)."""
import argparse
import os
from pathlib import Path
import re
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
FFI = ROOT / "bindings/ffi"
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "bindings/python"))
from fixtures import copy_fresh, target_directory
from stage_worker import stage, stage_notices

TARGET_RIDS = {
    "x86_64-pc-windows-msvc": "win-x64",
    "x86_64-unknown-linux-gnu": "linux-x64",
    "aarch64-apple-darwin": "osx-arm64",
    "x86_64-apple-darwin": "osx-x64",
}


def current_rid():
    target = os.environ.get("CARGO_BUILD_TARGET") or re.search(
        r"^host: (.+)$", subprocess.check_output(["rustc", "-vV"], cwd=ROOT, text=True), re.M
    ).group(1)
    try:
        return TARGET_RIDS[target]
    except KeyError:
        raise RuntimeError(f"unsupported NuGet native target: {target}") from None


def stage_native(rid=None):
    actual = current_rid()
    if rid is not None and rid != actual:
        raise ValueError(f"requested RID {rid} does not match Cargo target {actual}")
    destination = HERE / "VstAudio/runtimes" / actual / "native"
    subprocess.run(["cargo", "build", "--locked", "--release", "--manifest-path",
                    str(FFI / "Cargo.toml")], cwd=ROOT, check=True)
    worker = stage(destination=destination)
    library = {"win-x64": "vst_audio_ffi.dll", "linux-x64": "libvst_audio_ffi.so"}.get(
        actual, "libvst_audio_ffi.dylib"
    )
    copy_fresh(target_directory(workspace=FFI) / "release" / library, destination / library)
    stage_notices(destination, FFI / "Cargo.toml", "vst-audio-ffi-sys", "dependencies-ffi.json")
    return worker


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rid", choices=sorted(set(TARGET_RIDS.values())),
                        help="Require this Cargo target RID (does not cross-compile)")
    print(stage_native(parser.parse_args().rid))


if __name__ == "__main__":
    main()
