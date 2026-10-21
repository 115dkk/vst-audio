"""Build public fixtures, then exercise real worker processes. No external secrets."""
import os
from pathlib import Path
import subprocess
from fixtures import prepare, advanced_paths

ROOT = Path(__file__).resolve().parents[1]

def run(*args, env=None):
    subprocess.run(args, cwd=ROOT, env=env, check=True)

worker, fixture, vst3 = prepare()
advanced_vst2,advanced_vst3=advanced_paths()
env = dict(os.environ, VST_AUDIO_WORKER=str(worker),
           VST_AUDIO_ADVANCED_VST2=str(advanced_vst2), VST_AUDIO_ADVANCED_VST3=str(advanced_vst3),
           VST_AUDIO_TEST_VST2=str(fixture), VST_AUDIO_TEST_VST3=str(vst3))
run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "native",
    "--", "--ignored", "--test-threads=1", "--skip", "third_party_vst3", env=env)
run("cargo", "test", "--locked", "-p", "vst-audio-worker-sys", "--test", "lifecycle",
    "--", "--ignored", "--test-threads=1", env=env)
run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "application",
    "--", "--ignored", "--test-threads=1", env=env)

run("cargo", "test", "--locked", "-p", "vst-audio", "--test", "features", "--", "--ignored", "--test-threads=1", env=env)
