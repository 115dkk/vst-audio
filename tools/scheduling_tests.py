"""Build public fixtures, then check the worker audio thread's scheduling.

The caller's environment passes through, so VST_AUDIO_REQUIRE_REALTIME=1 reaches
the test once CI has raised RLIMIT_RTPRIO.
"""
import os
import subprocess
from fixtures import ROOT, prepare


def main():
    worker, vst2, _ = prepare()
    env = dict(os.environ, VST_AUDIO_WORKER=str(worker), VST_AUDIO_TEST_VST2=str(vst2))
    subprocess.run(["cargo", "test", "--locked", "-p", "vst-audio", "--test", "scheduling",
                    "--", "--ignored", "--nocapture", "--test-threads=1"],
                   cwd=ROOT, env=env, check=True)


if __name__ == "__main__":
    main()
