"""Unit tests for tools/binding_release.py (no network)."""
import io
import json
import unittest
import urllib.error

import binding_release as b


class Versions(unittest.TestCase):
    def test_recipe_follows_pyproject(self):
        recipe = (b.CONDA / "recipe.yaml").read_text(encoding="utf-8")
        self.assertEqual(b.recipe_version(recipe), b.python_version())

    def test_dotnet_version(self):
        self.assertRegex(b.dotnet_version(), r"^\d+\.\d+\.\d+$")


class Registry(unittest.TestCase):
    def test_pypi(self):
        self.assertTrue(b.published("python", "1.0.0", lambda url: b"{}"))
        def missing(url):
            raise urllib.error.HTTPError(url, 404, "Not Found", {}, io.BytesIO())
        self.assertFalse(b.published("python", "1.0.0", missing))

    def test_nuget(self):
        index = json.dumps({"versions": ["0.1.0", "0.2.0-beta"]}).encode()
        self.assertTrue(b.published("dotnet", "0.1.0", lambda url: index))
        self.assertTrue(b.published("dotnet", "0.2.0-Beta", lambda url: index))
        self.assertFalse(b.published("dotnet", "0.2.0", lambda url: index))

    def test_other_errors_propagate(self):
        def broken(url):
            raise urllib.error.HTTPError(url, 503, "Unavailable", {}, io.BytesIO())
        with self.assertRaises(urllib.error.HTTPError):
            b.published("python", "1.0.0", broken)


class StagedRecipe(unittest.TestCase):
    def test_source_becomes_the_tag_archive(self):
        recipe = (b.CONDA / "recipe.yaml").read_text(encoding="utf-8")
        staged = b.staged_recipe(recipe, "ab" * 32)
        self.assertIn("  url: https://github.com/115dkk/vst-audio/archive/refs/tags/"
                      "python-v${{ version }}.tar.gz\n", staged)
        self.assertIn("  sha256: " + "ab" * 32 + "\n", staged)
        for gone in ("path:", "use_gitignore", "filter:"):
            self.assertNotIn(gone, staged)
        # Everything outside the source block is unchanged.
        self.assertEqual(staged.split("\nbuild:", 1)[1], recipe.split("\nbuild:", 1)[1])
        self.assertEqual(staged.split("\nsource:", 1)[0], recipe.split("\nsource:", 1)[0])

    def test_rejects_a_bad_digest(self):
        with self.assertRaises(ValueError):
            b.staged_recipe("source:\n  path: x\nbuild:\n", "xyz")


if __name__ == "__main__":
    unittest.main()
