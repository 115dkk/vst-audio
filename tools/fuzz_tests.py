"""Bounded coverage-guided runs; retains only the public-input corpus/logs."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

from run import to_log

ROOT = Path(__file__).resolve().parents[1]
REPORT = ROOT / "test-results/fuzz"
CORPUS = ROOT / "fuzz/corpus/control"
GENERATED_PATTERN = "known-??-*.bin"
LEGACY_GENERATED_SEEDS = {"hello", "ack", "parameters", "image", "state"}



def _run_consumer(mode, directory):
    # The generator is a protocol example, so it builds from the workspace lock.
    result = subprocess.run([
        "cargo", "run", "--quiet", "--locked", "-p", "vst-audio-protocol", "--example",
        "control_seeds", "--", mode, str(directory),
    ], cwd=ROOT, capture_output=True, text=True)
    if result.stdout:
        print(result.stdout, end="", flush=True)
    if result.returncode:
        raise RuntimeError(f"control seed {mode} failed:\n{result.stderr[-12000:]}")


def install_generated_seeds(staging, corpus=CORPUS):
    """Replace only seeds owned by this generator, preserving other corpus files."""
    generated = sorted(staging.glob(GENERATED_PATTERN))
    names = [path.name for path in generated]
    if len(generated) != 32 or len(set(names)) != 32:
        raise RuntimeError("control seed generator must produce 32 unique files")
    corpus.mkdir(parents=True, exist_ok=True)
    stale = list(corpus.glob(GENERATED_PATTERN))
    stale.extend(corpus / name for name in LEGACY_GENERATED_SEEDS if (corpus / name).exists())
    for path in stale:
        if path.parent != corpus or not (path.is_file() or path.is_symlink()):
            raise RuntimeError(f"refusing to remove unexpected corpus path: {path}")
        path.unlink()
    for source in generated:
        shutil.copyfile(source, corpus / source.name)
    return names


def generate_control_corpus(corpus=CORPUS):
    temporary_parent = os.environ.get("RUNNER_TEMP")
    with tempfile.TemporaryDirectory(prefix="vst-audio-fuzz-seeds-", dir=temporary_parent) as temporary:
        staging = Path(temporary) / "generated"
        _run_consumer("generate", staging)
        _run_consumer("verify", staging)
        names = install_generated_seeds(staging, corpus)
        _run_consumer("verify", corpus)
    return names


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--generate-control-corpus-only", action="store_true")
    options = parser.parse_args()
    REPORT.mkdir(parents=True, exist_ok=True)
    generated = generate_control_corpus()
    if options.generate_control_corpus_only:
        print(f"installed {len(generated)} control corpus seeds", flush=True)
        return
    seconds = int(os.environ.get("VST_AUDIO_FUZZ_SECONDS", "120"))
    if not 1 <= seconds <= 3600:
        raise ValueError("fuzz duration must be 1..3600 seconds")
    results = []
    try:
        for target in ["control", "ring"]:
            command = ["cargo", "+nightly-2026-10-20", "fuzz", "run", target, "--",
                       "-max_total_time=" + str(seconds), "-max_len=65536",
                       "-rss_limit_mb=2048", "-print_final_stats=1"]
            result = to_log(command, REPORT / (target + ".log"), cwd=ROOT,
                            timeout=seconds + 300, tail_bytes=12000)
            print(result.tail, flush=True)
            units = re.search(r"stat::number_of_executed_units:\s*(\d+)", result.tail)
            executions = int(units.group(1)) if units else 0
            passed = result.returncode == 0 and not result.timed_out and executions > 100
            results.append({"target": target, "seconds": seconds, "executions": executions,
                            "timed_out": result.timed_out, "passed": passed})
            if not passed and result.returncode == 0:
                raise RuntimeError("fuzzer did not execute enough inputs")
            if result.returncode:
                raise SystemExit(result.returncode)
    finally:
        (REPORT / "summary.json").write_text(json.dumps(results, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
