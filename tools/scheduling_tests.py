"""Check realtime scheduling: the guard's unit tests, then the worker audio thread.

The caller's environment passes through, so VST_AUDIO_REQUIRE_REALTIME=1 reaches
both once CI has raised RLIMIT_RTPRIO; with it, a refused promotion fails instead
of taking the unprivileged branch.
"""
import os
import subprocess
from fixtures import ROOT, prepare


def main():
    subprocess.run(["cargo", "test", "--locked", "-p", "vst-audio-transport-sys", "realtime",
                    "--", "--nocapture"], cwd=ROOT, check=True)
    worker, vst2, _ = prepare()
    env = dict(os.environ, VST_AUDIO_WORKER=str(worker), VST_AUDIO_TEST_VST2=str(vst2))
    subprocess.run(["cargo", "test", "--locked", "-p", "vst-audio", "--test", "scheduling",
                    "--", "--ignored", "--nocapture", "--test-threads=1"],
                   cwd=ROOT, env=env, check=True)


if __name__ == "__main__":
    main()
