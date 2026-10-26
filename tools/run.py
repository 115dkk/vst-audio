"""Bounded subprocess execution with durable logs and diagnostic tails."""
from dataclasses import dataclass
from pathlib import Path
import subprocess


@dataclass(frozen=True)
class Result:
    returncode: int
    timed_out: bool
    tail: str


def to_log(command, log: Path, *, cwd, env=None, timeout: int, tail_bytes: int) -> Result:
    """Run a command to a file, kill it at the deadline, and retain its tail."""
    log.parent.mkdir(parents=True, exist_ok=True)
    timed_out = False
    with log.open("wb") as output:
        process = subprocess.Popen(
            list(map(str, command)), cwd=cwd, env=env, stdout=output,
            stderr=subprocess.STDOUT,
        )
        try:
            returncode = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            process.kill()
            returncode = process.wait()
    with log.open("rb") as output:
        output.seek(0, 2)
        output.seek(max(0, output.tell() - tail_bytes))
        tail = output.read().decode("utf-8", "replace")
    return Result(returncode, timed_out, tail)
