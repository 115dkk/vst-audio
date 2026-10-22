"""Run host-architecture tests against another architecture's worker.

On Linux invoke under xvfb-run, just like native_tests.py.
"""
import os
import subprocess
import sys
from fixtures import ROOT, prepare, advanced_paths


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: cross_arch_tests.py <Rust target>")
    target = sys.argv[1]
    architecture = {"i686": "x86", "x86_64": "x86_64", "aarch64": "aarch64"}[target.split("-")[0]]
    native_worker, _, _ = prepare()
    worker, vst2, vst3 = prepare(target)
    advanced_vst2, advanced_vst3 = advanced_paths(target)
    env = dict(os.environ, VST_AUDIO_WORKER=str(worker),
               VST_AUDIO_TEST_VST2=str(vst2), VST_AUDIO_TEST_VST3=str(vst3),
               VST_AUDIO_ADVANCED_VST2=str(advanced_vst2), VST_AUDIO_ADVANCED_VST3=str(advanced_vst3),
               VST_AUDIO_NATIVE_WORKER=str(native_worker), VST_AUDIO_CROSS_WORKER=str(worker),
               VST_AUDIO_CROSS_ARCH=architecture)
    for test in ("native", "features", "cross_architecture"):
        command = ["cargo", "test", "--locked", "-p", "vst-audio", "--test", test,
                   "--", "--ignored", "--test-threads=1"]
        if test == "native":
            command += ["--skip", "third_party_vst3"]
        if test == "cross_architecture":
            command += ["--nocapture", "cross_architecture_plugins_run_in_their_own_worker"]
        subprocess.run(command, cwd=ROOT, env=env, check=True)


if __name__ == "__main__":
    main()
