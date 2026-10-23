#!/usr/bin/env python3
"""Release gate of release.yml: decides whether a push to main releases, and
with which version.

Versions are plain semver tags ``v<major>.<minor>.<patch>``. While the major
version is 0 every GitHub release is a prerelease; no suffix is ever appended.

* the workspace version has no tag yet             -> release it as is
                                                      (first release, or a
                                                      manual bump), unless
                                                      it is too small (below)
* tagged, shipped files changed since that tag     -> bump, commit the bump to
                                                      main, release it
* tagged, nothing shipped changed                  -> do nothing

An untagged version is a bump whose release failed, or a manual bump. It is
released as is only when it is still right for what it would ship: at least the
bump the shipped commits since the latest lower tag ask for, and not yet on
crates.io (crates published by a run that failed later came from an older
commit, and the publish job skips crates already there). Otherwise the gate
bumps past it. Without this, a ``feat!`` that lands after a failed patch
release was published as that patch version.

The bump is minor when a commit that touched shipped files has a ``feat``
subject or says ``BREAKING CHANGE``, and patch otherwise. A file ships when it
is part of a published crate: ``SHIP`` patterns minus ``NOT_SHIPPED`` and
anything under ``tests``, ``benches`` or ``fuzz``, plus ``SHIP_ALWAYS``.

A bump rewrites the workspace version, the ``=<version>`` pins of the
workspace dependencies and the workspace packages in every Cargo.lock
(the root one, the GUI examples and fuzz), so ``--locked`` keeps working.
"""
from __future__ import annotations

import fnmatch
import os
import re
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

from fixtures import download

SHIP = (
    "crates/*",
    "apps/*",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "LICENSE",
    "LICENSE-ISC-DPF",
    "NOTICE",
)
# Every crate embeds the workspace README.
SHIP_ALWAYS = ("README.md",)
NOT_SHIPPED = ("*.md", "crates/vst-audio-testplugin/*", "crates/vst-audio-test-advanced/*")
NOT_SHIPPED_DIRS = ("tests", "benches", "fuzz", "examples")

TAG_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")
MINOR_SUBJECT_RE = re.compile(r"^(feat|minor)(\([^)]*\))?!?:")
BREAKING_RE = re.compile(r"^[a-z]+(\([^)]*\))?!:|BREAKING[ -]CHANGE", re.MULTILINE)

BRANCH = "main"
CARGO_TOML = Path("Cargo.toml")
LOCKS = (Path("Cargo.lock"), Path("examples/gui/Cargo.lock"), Path("fuzz/Cargo.lock"))
MEMBER_GLOBS = ("crates/*/Cargo.toml", "apps/*/Cargo.toml")


def _matches(path: str, patterns) -> bool:
    base = path.rsplit("/", 1)[-1]
    for pat in patterns:
        if pat.endswith("/*"):
            prefix = pat[:-2]
            if path == prefix or path.startswith(prefix + "/"):
                return True
        elif fnmatch.fnmatch(base, pat) or fnmatch.fnmatch(path, pat):
            return True
    return False


def ships(path: str) -> bool:
    if path in SHIP_ALWAYS:
        return True
    if not path or not _matches(path, SHIP) or _matches(path, NOT_SHIPPED):
        return False
    return not any(part in NOT_SHIPPED_DIRS for part in path.split("/")[:-1])


def parse_version(v: str) -> tuple[int, int, int]:
    m = re.match(r"^(\d+)\.(\d+)\.(\d+)$", v.strip())
    if not m:
        raise ValueError(f"not a plain major.minor.patch version: {v!r}")
    return tuple(int(x) for x in m.groups())


def latest_tag(tags) -> str | None:
    valid = [t for t in tags if TAG_RE.match(t)]
    return max(valid, key=lambda t: parse_version(t[1:])) if valid else None


def bump_kind(messages) -> str:
    for msg in messages:
        subject = msg.strip().splitlines()[0] if msg.strip() else ""
        if MINOR_SUBJECT_RE.match(subject) or BREAKING_RE.search(msg):
            return "minor"
    return "patch"


def bumped(version: str, kind: str) -> str:
    major, minor, patch = parse_version(version)
    if kind == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def lower_tag(version: str, tags) -> str | None:
    """The latest tag below ``version``."""
    return latest_tag(t for t in tags
                      if TAG_RE.match(t) and parse_version(t[1:]) < parse_version(version))


def decide(version: str, tags, shipped: list, messages, *, published=False):
    """Return ``(action, version)``: action is ``release``, ``bump`` or ``none``.

    For a tagged ``version``, ``shipped`` and ``messages`` describe the shipped
    changes since its tag. For an untagged one they describe those since the
    latest lower tag, and ``published`` says whether a crate of ``version`` is
    already on crates.io.
    """
    if f"v{version}" not in tags:
        candidate = version
        lower = lower_tag(version, tags)
        if lower and shipped:
            needed = bumped(lower[1:], bump_kind(messages))
            if parse_version(needed) > parse_version(candidate):
                candidate = needed
        if published and candidate == version:
            candidate = bumped(version, "patch")
        if candidate == version:
            return "release", version
    else:
        if not shipped:
            return "none", version
        candidate = bumped(version, bump_kind(messages))
    while f"v{candidate}" in tags:
        candidate = bumped(candidate, "patch")
    return "bump", candidate


def published_on_crates_io(names, version: str) -> bool:
    """Whether any of ``names`` already has ``version`` on crates.io."""
    agent = f"vst-audio release gate (https://github.com/{os.environ.get('GITHUB_REPOSITORY', '')})"
    for name in sorted(names):
        request = urllib.request.Request(
            f"https://crates.io/api/v1/crates/{name}/{version}", headers={"User-Agent": agent})
        try:
            download(request)
        except urllib.error.HTTPError as error:
            if error.code == 404:
                continue
            raise
        print(f"{name} {version} is already on crates.io")
        return True
    return False


def workspace_version(cargo_toml: str) -> str:
    section = None
    for line in cargo_toml.splitlines():
        header = re.match(r"^\s*\[([^\]]+)\]\s*$", line)
        if header:
            section = header.group(1).strip()
            continue
        m = re.match(r'^\s*version\s*=\s*"([^"]+)"', line)
        if m and section == "workspace.package":
            return m.group(1)
    raise ValueError("no [workspace.package] version in Cargo.toml")


def set_workspace_version(cargo_toml: str, members, old: str, new: str) -> str:
    """Rewrite ``[workspace.package].version`` and the ``=old`` pins of members."""
    lines = cargo_toml.splitlines(keepends=True)
    section, package, pins = None, False, 0
    for i, line in enumerate(lines):
        header = re.match(r"^\s*\[([^\]]+)\]\s*$", line)
        if header:
            section = header.group(1).strip()
            continue
        if section == "workspace.package" and re.match(r'^\s*version\s*=\s*"', line):
            lines[i] = re.sub(r'"[^"]+"', f'"{new}"', line, count=1)
            package = True
        elif section == "workspace.dependencies":
            name = re.match(r"^\s*([A-Za-z0-9_-]+)\s*=", line)
            if name and name.group(1) in members and f'"={old}"' in line:
                lines[i] = line.replace(f'"={old}"', f'"={new}"')
                pins += 1
    if not package or pins == 0:
        raise ValueError("Cargo.toml has no workspace version or member pins")
    return "".join(lines)


def set_lock_versions(lock: str, members, old: str, new: str) -> str:
    """Rewrite ``old`` to ``new`` for path packages named in ``members``.

    Registry and git packages carry a ``source`` line right after ``version``;
    path packages do not.
    """
    pattern = re.compile(
        r'(?m)^(\[\[package\]\]\r?\nname = "([^"]+)"\r?\nversion = ")'
        + re.escape(old) + r'("\r?\n)(?!source = )'
    )

    def replace(m):
        if m.group(2) not in members:
            return m.group(0)
        return m.group(1) + new + m.group(3)

    return pattern.sub(replace, lock)


def _run(*args, check=True):
    return subprocess.run(
        list(args), check=check, capture_output=True, text=True,
        encoding="utf-8", errors="replace",
    )


def _git(*args) -> str:
    return _run("git", *args).stdout.strip()


def _read(path: Path) -> str:
    with open(path, encoding="utf-8", newline="") as fh:
        return fh.read()


def _write(path: Path, text: str) -> None:
    with open(path, "w", encoding="utf-8", newline="") as fh:
        fh.write(text)


def workspace_members() -> set[str]:
    """Names of the members that inherit the workspace version."""
    names = set()
    for glob in MEMBER_GLOBS:
        for manifest in Path().glob(glob):
            text = _read(manifest)
            if re.search(r"(?m)^version\.workspace\s*=\s*true", text):
                names.add(re.search(r'(?m)^name\s*=\s*"([^"]+)"', text).group(1))
    return names


def _commit_bump(old: str, new: str) -> str:
    members = workspace_members()
    _write(CARGO_TOML, set_workspace_version(_read(CARGO_TOML), members, old, new))
    for lock in LOCKS:
        if lock.exists():
            _write(lock, set_lock_versions(_read(lock), members, old, new))
    _run("git", "config", "user.name", "github-actions[bot]")
    _run("git", "config", "user.email",
         "41898282+github-actions[bot]@users.noreply.github.com")
    _run("git", "add", str(CARGO_TOML), *(str(lock) for lock in LOCKS if lock.exists()))
    _run("git", "commit", "-m", f"chore(release): v{new}")
    for attempt in range(1, 4):
        # A push made with the workflow token starts no workflow, so the bump
        # commit does not release itself.
        push = _run("git", "push", "origin", f"HEAD:{BRANCH}", check=False)
        if push.returncode == 0:
            return _git("rev-parse", "HEAD")
        print(f"::warning::bump push rejected (attempt {attempt})\n{push.stderr}")
        _run("git", "fetch", "origin", BRANCH, check=False)
        if _run("git", "rebase", "FETCH_HEAD", check=False).returncode != 0:
            _run("git", "rebase", "--abort", check=False)
            raise SystemExit(f"{BRANCH} moved in a way the bump cannot rebase onto")
        time.sleep(5)
    raise SystemExit("bump push failed after 3 attempts")


def main() -> int:
    head = os.environ.get("GITHUB_SHA") or _git("rev-parse", "HEAD")
    _run("git", "fetch", "--tags", "--force", "--quiet", "origin", check=False)
    tags = set(_git("tag", "-l", "v*").splitlines())
    version = workspace_version(_read(CARGO_TOML))
    current_tag = f"v{version}"
    previous = latest_tag(tags)

    tagged = current_tag in tags
    base = current_tag if tagged else lower_tag(version, tags)
    shipped, messages = [], []
    if base:
        changed = _git("diff", "--name-only", base, head).splitlines()
        shipped = [f for f in changed if ships(f)]
        if shipped:
            log = _git("log", "--no-merges", "--format=%B%x00", f"{base}..{head}",
                       "--", *shipped)
            messages = [m for m in log.split("\x00") if m.strip()]
    published = not tagged and published_on_crates_io(workspace_members(), version)

    action, new_version = decide(version, tags, shipped, messages, published=published)
    print(f"head={head[:12]} version={version} tagged={tagged} base={base} "
          f"published={published} previous={previous}")
    print(f"shipped changes: {shipped[:20]}")
    print(f"decision: {action} {new_version}")

    release_sha = head
    if action == "bump":
        if os.environ.get("GITHUB_REF", f"refs/heads/{BRANCH}") != f"refs/heads/{BRANCH}":
            raise SystemExit(f"a bump commits to {BRANCH}")
        release_sha = _commit_bump(version, new_version)
        print(f"bumped to v{new_version}; release_sha={release_sha}")

    payload = (
        f"should_release={'false' if action == 'none' else 'true'}\n"
        f"version={new_version}\n"
        f"tag=v{new_version}\n"
        f"prerelease={'true' if parse_version(new_version)[0] == 0 else 'false'}\n"
        f"previous_tag={previous or ''}\n"
        f"release_sha={release_sha}\n"
    )
    out = os.environ.get("GITHUB_OUTPUT")
    if out:
        with open(out, "a", encoding="utf-8") as fh:
            fh.write(payload)
    print(payload, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
