"""Build public fixtures, then exercise real worker processes. No external secrets."""
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]

def run(*args, env=None):
    subprocess.run(args, cwd=ROOT, env=env, check=True)

run("cargo", "build", "--locked", "-p", "vst-audio-worker", "-p", "vst-audio-testplugin")
suffix = ".exe" if sys.platform == "win32" else ""
library = {
    "win32": "vst_audio_testplugin.dll",
    "darwin": "libvst_audio_testplugin.dylib",
}.get(sys.platform, "libvst_audio_testplugin.so")
directory = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "debug"
worker = directory / ("vst-audio-worker" + suffix)
fixture = directory / library
if not worker.is_file() or not fixture.is_file():
    raise SystemExit("required native fixture or worker missing")
vst3 = directory / "vst_audio_testplugin.vst3"
shutil.copyfile(fixture, vst3)
env = dict(os.environ, VST_AUDIO_WORKER=str(worker),
           VST_AUDIO_TEST_VST2=str(fixture), VST_AUDIO_TEST_VST3=str(vst3))
run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "native",
    "--", "--ignored", "--test-threads=1", "--skip", "third_party_vst3", env=env)
run("cargo", "test", "--locked", "-p", "vst-audio-worker-sys", "--test", "lifecycle",
    "--", "--ignored", "--test-threads=1", env=env)
run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "application",
    "--", "--ignored", "--test-threads=1", env=env)
