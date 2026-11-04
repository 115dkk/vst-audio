"""Build fixtures and a bundled wheel, then test it in a fresh venv (Python 3.11+).

``--wheel PATH`` tests an already built wheel instead, such as the manylinux
wheel CI builds in its container.
"""
import argparse
import os
from pathlib import Path
import subprocess
import sys
import venv

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "tools"))
from fixtures import advanced_paths, prepare
from stage_worker import stage


def run(*args, env=None):
    subprocess.run(list(map(str, args)), cwd=HERE, env=env, check=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--wheel", type=Path, help="test this wheel instead of building one")
    wheel = parser.parse_args().wheel
    worker, vst2, vst3 = prepare()
    if wheel is None:
        worker = stage()
    environment = ROOT / "target/python-venv"
    venv.EnvBuilder(with_pip=True, clear=True).create(environment)
    python = environment / ("Scripts/python.exe" if sys.platform == "win32" else "bin/python")
    run(python, "-m", "pip", "install", "maturin==1.16.0", "pytest", "numpy>=1.21")
    if wheel is None:
        # Build, rather than develop, to exercise the actual bundled wheel layout.
        wheels = HERE / "target/test-wheels"
        run(python, "-m", "maturin", "build", "--locked", "--release", "--interpreter", python,
            "--out", wheels)
        wheel = max(wheels.glob("vst_audio-0.1.0-*.whl"), key=lambda path: path.stat().st_mtime_ns)
    run(python, "-m", "pip", "install", "--no-deps", wheel.resolve())
    advanced_vst2, advanced_vst3 = advanced_paths()
    env = dict(os.environ, VST_AUDIO_WORKER=str(worker),
               VST_AUDIO_TEST_VST2=str(vst2), VST_AUDIO_TEST_VST3=str(vst3),
               VST_AUDIO_ADVANCED_VST2=str(advanced_vst2),
               VST_AUDIO_ADVANCED_VST3=str(advanced_vst3))
    # Test the installed package, not the source checkout.
    env.pop("PYTHONPATH", None)
    run(python, "-m", "pytest", "-v", "tests", env=env)


if __name__ == "__main__":
    main()
