import tempfile
from pathlib import Path
import unittest

from fuzz_tests import install_generated_seeds


class InstallGeneratedSeedsTests(unittest.TestCase):
    def test_replaces_owned_seeds_without_collisions_or_unrelated_deletion(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            staging = root / "staging"
            corpus = root / "corpus"
            staging.mkdir()
            corpus.mkdir()
            for kind in range(1, 33):
                (staging / f"known-{kind:02}-kind-{kind}.bin").write_bytes(bytes([kind]))
            (corpus / "known-01-stale.bin").write_bytes(b"stale")
            (corpus / "hello").write_bytes(b"legacy")
            (corpus / "user-seed").write_bytes(b"keep")

            names = install_generated_seeds(staging, corpus)

            self.assertEqual(len(names), 32)
            self.assertEqual(len(set(names)), 32)
            self.assertFalse((corpus / "known-01-stale.bin").exists())
            self.assertFalse((corpus / "hello").exists())
            self.assertEqual((corpus / "user-seed").read_bytes(), b"keep")
            self.assertEqual(len(list(corpus.glob("known-??-*.bin"))), 32)

    def test_rejects_incomplete_generation_before_removing_stale_seeds(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            staging = root / "staging"
            corpus = root / "corpus"
            staging.mkdir()
            corpus.mkdir()
            (staging / "known-01-only.bin").write_bytes(b"one")
            stale = corpus / "known-01-stale.bin"
            stale.write_bytes(b"stale")

            with self.assertRaises(RuntimeError):
                install_generated_seeds(staging, corpus)

            self.assertEqual(stale.read_bytes(), b"stale")


if __name__ == "__main__":
    unittest.main()
