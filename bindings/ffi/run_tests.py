"""Build native fixtures and run the ignored C ABI integration tests."""
import os
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "tools"))
from fixtures import prepare


def main():
    worker, vst2, vst3 = prepare()
    environment = dict(
        os.environ,
        VST_AUDIO_WORKER=str(worker),
        VST_AUDIO_TEST_VST2=str(vst2),
        VST_AUDIO_TEST_VST3=str(vst3),
    )
    subprocess.run(
        [
            "cargo",
            "test",
            "--locked",
            "--manifest-path",
            HERE / "Cargo.toml",
            "--",
            "--ignored",
            "--test-threads=1",
        ],
        cwd=ROOT,
        env=environment,
        check=True,
    )


if __name__ == "__main__":
    main()
