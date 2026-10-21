"""Public native fixture layouts, shared by functional and GUI consumers."""
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys
import uuid

ROOT = Path(__file__).resolve().parents[1]

def copy_fresh(source, target):
    # A new inode avoids macOS retaining a code-signature cache for overwritten
    # dylib pages when a cached fixture was previously loaded.
    staged=target.with_name(target.name+"."+uuid.uuid4().hex)
    shutil.copyfile(source,staged)
    os.replace(staged,target)

def prepare():
    subprocess.run(["cargo", "build", "--locked", "-p", "vst-audio-worker",
                    "-p", "vst-audio-testplugin", "-p", "vst-audio-test-advanced"], cwd=ROOT, check=True)
    directory = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "debug"
    suffix = ".exe" if sys.platform == "win32" else ""
    source = directory / {
        "win32": "vst_audio_testplugin.dll",
        "darwin": "libvst_audio_testplugin.dylib",
    }.get(sys.platform, "libvst_audio_testplugin.so")
    worker = directory / ("vst-audio-worker" + suffix)
    if not source.is_file() or not worker.is_file():
        raise RuntimeError("native worker or fixture missing")
    bundle = directory / "native-fixtures" / "Matrix.vst3"
    if sys.platform == "darwin":
        target = bundle / "Contents" / "MacOS" / "Matrix"
    elif sys.platform == "win32":
        target = bundle / "Contents" / "x86_64-win" / "Matrix.vst3"
    else:
        arch = "aarch64" if platform.machine() in ("aarch64", "arm64") else "x86_64"
        target = bundle / "Contents" / f"{arch}-linux" / "Matrix.so"
    target.parent.mkdir(parents=True, exist_ok=True)
    copy_fresh(source, target)
    vst2 = source
    if sys.platform == "darwin":
        info = {
            "CFBundleExecutable": "Matrix", "CFBundleIdentifier": "org.vst-audio.fixture",
            "CFBundlePackageType": "BNDL", "CFBundleVersion": "1",
        }
        (bundle / "Contents" / "Info.plist").write_bytes(plistlib.dumps(info))
        subprocess.run(["codesign", "--force", "--sign", "-", str(bundle)], check=True)
        vst2 = directory / "native-fixtures" / "Matrix.vst"
        executable = vst2 / "Contents" / "MacOS" / "Matrix"
        executable.parent.mkdir(parents=True, exist_ok=True)
        copy_fresh(source, executable)
        (vst2 / "Contents" / "Info.plist").write_bytes(plistlib.dumps(info))
        subprocess.run(["codesign", "--force", "--sign", "-", str(vst2)], check=True)
    advanced_source=directory / source.name.replace("testplugin","test_advanced")
    advanced_bundle=directory / "native-fixtures" / "Advanced.vst3"
    advanced_target=advanced_bundle / target.relative_to(bundle)
    # Platform executable names must match their own bundle names.
    advanced_target=advanced_target.with_name(advanced_target.name.replace("Matrix","Advanced"))
    advanced_target.parent.mkdir(parents=True,exist_ok=True)
    copy_fresh(advanced_source,advanced_target)
    if sys.platform=="darwin":
        advanced_info=dict(info,CFBundleExecutable="Advanced",CFBundleIdentifier="org.vst-audio.advanced")
        (advanced_bundle/"Contents"/"Info.plist").write_bytes(plistlib.dumps(advanced_info))
        subprocess.run(["codesign","--force","--sign","-",str(advanced_bundle)],check=True)
    return worker, vst2, bundle

def advanced_paths():
    directory=Path(os.environ.get("CARGO_TARGET_DIR",ROOT/"target"))/"debug"
    source=directory/{"win32":"vst_audio_test_advanced.dll","darwin":"libvst_audio_test_advanced.dylib"}.get(sys.platform,"libvst_audio_test_advanced.so")
    return source,directory/"native-fixtures"/"Advanced.vst3"
