"""Exercise an installed conda package against the repository's native fixtures."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import xml.etree.ElementTree as ET

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(ROOT / "tools"))
from fixtures import advanced_paths, prepare  # noqa: E402


def run(command: list[os.PathLike[str] | str], *, env: dict[str, str] | None = None,
        check: bool = True, capture_output: bool = False) -> subprocess.CompletedProcess[str]:
    printable = " ".join(map(str, command))
    print(f"+ {printable}", flush=True)
    return subprocess.run(
        list(map(str, command)), cwd=ROOT, env=env, check=check, text=True,
        capture_output=capture_output,
    )


def installed_paths(python: Path) -> tuple[Path, Path, Path]:
    script = r"""
import importlib.util, json, os, pathlib, sys
os.environ.pop("VST_AUDIO_WORKER", None)
import vst_audio
spec = importlib.util.find_spec("vst_audio._native")
print(json.dumps({
    "prefix": sys.prefix,
    "worker": str(vst_audio.Host().worker),
    "extension": spec.origin,
}))
"""
    result = run([python, "-c", script], capture_output=True)
    data = json.loads(result.stdout.strip().splitlines()[-1])
    return tuple(Path(data[name]).resolve() for name in ("prefix", "worker", "extension"))  # type: ignore[return-value]


def require_inside(path: Path, directory: Path) -> None:
    try:
        path.relative_to(directory)
    except ValueError as error:
        raise RuntimeError(f"installed worker {path} is outside environment prefix {directory}") from error


def junit_counts(path: Path) -> tuple[int, int]:
    root = ET.parse(path).getroot()
    suites = [root] if root.tag == "testsuite" else list(root.iter("testsuite"))
    # A testsuites root can contain aggregate and nested suites. Pytest emits only
    # direct testsuite children, but counting testcase nodes avoids double counting.
    tests = len(list(root.iter("testcase")))
    skipped = len(list(root.iter("skipped")))
    if not suites or tests < 1:
        raise RuntimeError(f"JUnit report contains no tests: {path}")
    return tests, skipped


def glibc_versions(path: Path) -> list[tuple[int, int]]:
    commands = (["objdump", "-T", path], ["readelf", "--version-info", path])
    for command in commands:
        try:
            result = run(command, check=False, capture_output=True)
        except FileNotFoundError:
            continue
        if result.returncode != 0:
            continue
        versions = {
            tuple(map(int, match.groups()))
            for match in re.finditer(r"GLIBC_(\d+)\.(\d+)", result.stdout)
        }
        return sorted(versions)
    raise RuntimeError(f"neither objdump nor readelf could inspect {path}")


def check_glibc(paths: list[Path], maximum: tuple[int, int]) -> None:
    for path in paths:
        versions = glibc_versions(path)
        highest = versions[-1] if versions else (0, 0)
        print(f"highest GLIBC symbol for {path}: {highest[0]}.{highest[1]}")
        if highest > maximum:
            raise RuntimeError(
                f"{path} requires GLIBC_{highest[0]}.{highest[1]}, above "
                f"allowed GLIBC_{maximum[0]}.{maximum[1]}"
            )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", required=True, type=Path,
                        help="Python executable in the conda test environment")
    parser.add_argument("--max-glibc", default="2.28",
                        help="highest accepted Linux GLIBC symbol version")
    args = parser.parse_args()
    python = args.python.resolve()
    if not python.is_file():
        parser.error(f"Python executable does not exist: {python}")
    try:
        maximum = tuple(map(int, args.max_glibc.split(".")))
        if len(maximum) != 2:
            raise ValueError
    except ValueError:
        parser.error("--max-glibc must be MAJOR.MINOR")

    _fixture_worker, vst2, vst3 = prepare()
    advanced_vst2, advanced_vst3 = advanced_paths()
    prefix, worker, extension = installed_paths(python)
    require_inside(worker, prefix)
    if not worker.is_file():
        raise RuntimeError(f"bundled worker is not a file: {worker}")
    print(f"environment prefix: {prefix}")
    print(f"bundled worker: {worker}")
    print(f"installed extension: {extension}")
    run([worker, "--help"])

    report = ROOT / "target" / "conda-test-results.xml"
    report.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env.update({
        "VST_AUDIO_WORKER": str(worker),
        "VST_AUDIO_TEST_VST2": str(vst2),
        "VST_AUDIO_TEST_VST3": str(vst3),
        "VST_AUDIO_ADVANCED_VST2": str(advanced_vst2),
        "VST_AUDIO_ADVANCED_VST3": str(advanced_vst3),
    })
    env.pop("PYTHONPATH", None)
    pytest = run(
        [python, "-m", "pytest", "-v", "bindings/python/tests",
         f"--junitxml={report}"], env=env, check=False,
    )
    tests, skipped = junit_counts(report)
    print(f"JUnit summary: {tests} tests, {skipped} skipped")
    if pytest.returncode:
        raise subprocess.CalledProcessError(pytest.returncode, pytest.args)
    if skipped:
        raise RuntimeError(f"native package test suite skipped {skipped} test(s)")

    if sys.platform.startswith("linux"):
        check_glibc([worker, extension], maximum)  # type: ignore[arg-type]
    elif sys.platform == "darwin":
        # conda relinks binaries after the build; the worker's bundle must still
        # carry the valid ad-hoc signature stage_worker.py gave it.
        bundle = next(parent for parent in worker.parents if parent.suffix == ".app")
        run(["codesign", "--verify", "--strict", "--verbose=2", bundle])


if __name__ == "__main__":
    main()
