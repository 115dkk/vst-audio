"""Unit tests of the pure parts of release_gate.py."""
import contextlib
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

import release_gate as g

TOML = '''[workspace.package]
version = "0.1.0"

[workspace.dependencies]
vst-audio = { path = "crates/vst-audio", version = "=0.1.0" }
vst-audio-ring = { path = "crates/vst-audio-ring", version = "=0.1.0" }
vst3 = "=0.1.0"
'''

LOCK = '''[[package]]
name = "vst-audio"
version = "0.1.0"
dependencies = [
 "vst-audio-ring",
]

[[package]]
name = "vst-audio-gui-example"
version = "0.1.0"

[[package]]
name = "vst3"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
'''


class Gate(unittest.TestCase):
    def test_ships(self):
        self.assertTrue(g.ships("crates/vst-audio/src/lib.rs"))
        self.assertTrue(g.ships("README.md"))
        self.assertTrue(g.ships("Cargo.lock"))
        self.assertFalse(g.ships("crates/vst-audio/tests/native.rs"))
        self.assertFalse(g.ships("crates/vst-audio-testplugin/src/lib.rs"))
        self.assertFalse(g.ships("crates/vst-audio-test-advanced/src/lib.rs"))
        self.assertFalse(g.ships("crates/vst-audio-test-window/src/lib.rs"))
        self.assertFalse(g.ships("docs/roadmap.md"))
        self.assertFalse(g.ships(".github/workflows/ci.yml"))

    def test_decide(self):
        self.assertEqual(g.decide("0.1.0", set(), [], []), ("release", "0.1.0"))
        self.assertEqual(g.decide("0.1.0", {"v0.1.0"}, [], []), ("none", "0.1.0"))
        self.assertEqual(g.decide("0.1.0", {"v0.1.0"}, ["Cargo.toml"], ["fix: x"]),
                         ("bump", "0.1.1"))
        self.assertEqual(g.decide("0.1.0", {"v0.1.0"}, ["Cargo.toml"], ["feat(ring): x"]),
                         ("bump", "0.2.0"))
        self.assertEqual(g.decide("0.1.0", {"v0.1.0", "v0.1.1"}, ["a"], ["fix: x"]),
                         ("bump", "0.1.2"))

    def test_decide_untagged_version(self):
        # A manual bump, or a failed patch release retried by a later fix.
        self.assertEqual(g.decide("0.3.0", {"v0.2.0"}, ["a"], ["feat!: x"]),
                         ("release", "0.3.0"))
        self.assertEqual(g.decide("0.2.1", {"v0.2.0"}, ["a"], ["fix: x", "test: y"]),
                         ("release", "0.2.1"))
        # A failed patch release followed by a breaking change.
        self.assertEqual(g.decide("0.2.1", {"v0.2.0"}, ["a"], ["test: y", "feat!: x"]),
                         ("bump", "0.3.0"))
        # Crates of the version are on crates.io from an earlier commit.
        self.assertEqual(g.decide("0.2.1", {"v0.2.0"}, ["a"], ["fix: x"], published=True),
                         ("bump", "0.2.2"))
        self.assertEqual(g.decide("0.2.1", {"v0.2.0"}, ["a"], ["feat!: x"], published=True),
                         ("bump", "0.3.0"))
        self.assertEqual(g.decide("0.1.0", set(), [], [], published=True), ("bump", "0.1.1"))
        self.assertEqual(g.lower_tag("0.2.1", {"v0.1.9", "v0.2.0", "v0.3.0", "x"}), "v0.2.0")

    def test_bump_rewrites_members_only(self):
        members = {"vst-audio", "vst-audio-ring"}
        toml = g.set_workspace_version(TOML, members, "0.1.0", "0.2.0")
        self.assertEqual(g.workspace_version(toml), "0.2.0")
        self.assertIn('vst-audio-ring = { path = "crates/vst-audio-ring", version = "=0.2.0" }', toml)
        self.assertIn('vst3 = "=0.1.0"', toml)
        lock = g.set_lock_versions(LOCK, members, "0.1.0", "0.2.0")
        self.assertIn('name = "vst-audio"\nversion = "0.2.0"', lock)
        self.assertIn('name = "vst-audio-gui-example"\nversion = "0.1.0"', lock)
        self.assertIn('name = "vst3"\nversion = "0.1.0"', lock)

    def test_failed_tag_fetch_exits(self):
        failed = subprocess.CompletedProcess([], 1, "", "network unavailable")
        with mock.patch.object(g, "_run", return_value=failed) as run, \
                mock.patch.object(g.time, "sleep"):
            with self.assertRaisesRegex(SystemExit, "tag fetch failed after 3 attempts"):
                g._fetch_tags()
        self.assertEqual(run.call_count, 3)

    def _release_with_pushes(self, push_results, remote_main):
        """Runs main() for a bump whose pushes return `push_results` in turn."""
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            results = iter(push_results)
            pushes = 0

            def run(*args, **_kwargs):
                nonlocal pushes
                if args[:3] == ("git", "push", "origin"):
                    pushes += 1
                    code, stderr = next(results)
                    return subprocess.CompletedProcess([], code, "", stderr)
                if args[:3] == ("git", "ls-remote", "origin"):
                    return subprocess.CompletedProcess([], 0, f"{remote_main}\trefs/heads/main\n", "")
                return subprocess.CompletedProcess([], 0, "", "")

            def git(*args):
                if args[:2] == ("tag", "-l"):
                    return "v0.1.0"
                if args[:2] == ("diff", "--name-only"):
                    return "Cargo.toml"
                if args[0] == "log":
                    return "fix: change"
                return "b" * 40

            with mock.patch.dict(os.environ, {
                "GITHUB_SHA": "a" * 40,
                "GITHUB_REF": "refs/heads/main",
                "GITHUB_OUTPUT": str(output),
            }, clear=False), mock.patch.object(g, "CARGO_TOML", Path(directory) / "Cargo.toml"), \
                    mock.patch.object(g, "LOCKS", ()), \
                    mock.patch.object(g, "_read", return_value=TOML), \
                    mock.patch.object(g, "workspace_members", return_value={"vst-audio", "vst-audio-ring"}), \
                    mock.patch.object(g, "published_on_crates_io", return_value=False), \
                    mock.patch.object(g, "_write"), \
                    mock.patch.object(g, "_git", side_effect=git), \
                    mock.patch.object(g, "_fetch_tags"), \
                    mock.patch.object(g, "_run", side_effect=run), \
                    mock.patch.object(g.time, "sleep"):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(g.main(), 0)
            return output.read_text(), pushes

    def test_rejected_bump_push_defers_release_to_new_main(self):
        output, pushes = self._release_with_pushes([(1, "non-fast-forward")], "c" * 40)
        self.assertIn("should_release=false", output)
        self.assertEqual(pushes, 1)

    def test_transient_push_failure_retries_while_main_is_unmoved(self):
        # A network failure with main still at the decided head is not a moved main.
        output, pushes = self._release_with_pushes(
            [(128, "Could not resolve host"), (0, "")], "a" * 40)
        self.assertIn("should_release=true", output)
        self.assertIn(f"release_sha={'b' * 40}", output)
        self.assertEqual(pushes, 2)

    def test_push_failing_with_main_unmoved_exits(self):
        with self.assertRaisesRegex(SystemExit, "bump push failed after 3 attempts"):
            self._release_with_pushes([(128, "Could not resolve host")] * 3, "a" * 40)

if __name__ == "__main__":
    unittest.main()
