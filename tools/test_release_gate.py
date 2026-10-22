"""Unit tests of the pure parts of release_gate.py."""
import unittest

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


if __name__ == "__main__":
    unittest.main()
