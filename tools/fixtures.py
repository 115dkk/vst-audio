"""Public native fixture layouts, shared by functional and GUI consumers."""
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]

def download(url, timeout=30):
    """Fetch `url`, retrying network failures and 5xx/429 answers four times.

    Hosted runners lose DNS or connections now and then; a release must not
    stop on one. Other HTTP errors (a 404 included) are raised at once."""
    for delay in (2, 4, 8, 16, None):
        try:
            with urllib.request.urlopen(url, timeout=timeout) as response:
                return response.read()
        except urllib.error.HTTPError as error:
            if delay is None or not (error.code == 429 or error.code >= 500):
                raise
        except OSError:
            if delay is None:
                raise
        time.sleep(delay)

def copy_fresh(source, target):
    # A new inode avoids macOS retaining a code-signature cache for overwritten
    # dylib pages when a cached fixture was previously loaded.
    staged=target.with_name(target.name+"."+uuid.uuid4().hex)
    shutil.copyfile(source,staged)
    os.replace(staged,target)

def target_directory(target=None):
    directory=Path(os.environ.get("CARGO_TARGET_DIR",ROOT/"target"))
    target=target or os.environ.get("CARGO_BUILD_TARGET")
    return directory/target if target else directory

def target_platform(target=None):
    if target is None:
        return sys.platform
    if "windows" in target:
        return "win32"
    if "apple-darwin" in target:
        return "darwin"
    if "linux" in target:
        return "linux"
    raise ValueError(f"unsupported fixture target: {target}")

def bundle_folder(target=None):
    if target:
        return {
            "i686-pc-windows-msvc": "x86-win",
            "x86_64-pc-windows-msvc": "x86_64-win",
            "aarch64-pc-windows-msvc": "arm64-win",
            "i686-unknown-linux-gnu": "i386-linux",
            "x86_64-unknown-linux-gnu": "x86_64-linux",
            "aarch64-unknown-linux-gnu": "aarch64-linux",
        }[target]
    if sys.platform == "win32":
        return "x86_64-win"
    arch = "aarch64" if platform.machine() in ("aarch64", "arm64") else "x86_64"
    return f"{arch}-linux"

def prepare(target=None):
    command = ["cargo", "build", "--locked"]
    if target:
        command += ["--target", target]
    subprocess.run(command + ["-p", "vst-audio-worker", "-p", "vst-audio-testplugin",
                             "-p", "vst-audio-test-advanced"], cwd=ROOT, check=True)
    directory = target_directory(target) / "debug"
    os_platform = target_platform(target)
    suffix = ".exe" if os_platform == "win32" else ""
    source = directory / {
        "win32": "vst_audio_testplugin.dll",
        "darwin": "libvst_audio_testplugin.dylib",
    }.get(os_platform, "libvst_audio_testplugin.so")
    worker = directory / ("vst-audio-worker" + suffix)
    if not source.is_file() or not worker.is_file():
        raise RuntimeError("native worker or fixture missing")
    bundle = directory / "native-fixtures" / "Matrix.vst3"
    if os_platform == "darwin":
        binary = bundle / "Contents" / "MacOS" / "Matrix"
    else:
        name = "Matrix.vst3" if os_platform == "win32" else "Matrix.so"
        binary = bundle / "Contents" / bundle_folder(target) / name
    binary.parent.mkdir(parents=True, exist_ok=True)
    copy_fresh(source, binary)
    vst2 = source
    if os_platform == "darwin":
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
    advanced_target=advanced_bundle / binary.relative_to(bundle)
    # Platform executable names must match their own bundle names.
    advanced_target=advanced_target.with_name(advanced_target.name.replace("Matrix","Advanced"))
    advanced_target.parent.mkdir(parents=True,exist_ok=True)
    copy_fresh(advanced_source,advanced_target)
    if os_platform=="darwin":
        advanced_info=dict(info,CFBundleExecutable="Advanced",CFBundleIdentifier="org.vst-audio.advanced")
        (advanced_bundle/"Contents"/"Info.plist").write_bytes(plistlib.dumps(advanced_info))
        subprocess.run(["codesign","--force","--sign","-",str(advanced_bundle)],check=True)
    return worker, vst2, bundle

def advanced_paths(target=None):
    directory=target_directory(target)/"debug"
    source=directory/{"win32":"vst_audio_test_advanced.dll","darwin":"libvst_audio_test_advanced.dylib"}.get(target_platform(target),"libvst_audio_test_advanced.so")
    return source,directory/"native-fixtures"/"Advanced.vst3"
