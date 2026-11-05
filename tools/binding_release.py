"""Versions, registry checks and the conda-forge submission for the language bindings.

The bindings carry their own versions (ADR 0006): the Python one in
``bindings/python/pyproject.toml``, the .NET one in ``bindings/dotnet/Version.props``.
release.yml publishes both on every push to main and skips versions already in
the registry, then tags a version this run actually published as
``python-v<version>`` or ``dotnet-v<version>``. The Python tag's GitHub archive is
the source conda-forge builds from; ``staged-recipe`` writes the recipe to submit
to conda-forge/staged-recipes, with that archive's URL and sha256.

    python3 tools/binding_release.py version python
    python3 tools/binding_release.py published dotnet     # prints true or false
    python3 tools/binding_release.py staged-recipe --out DIR [--sha256 HEX]
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import sys
import tomllib
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
PYPROJECT = ROOT / "bindings/python/pyproject.toml"
DOTNET_PROPS = ROOT / "bindings/dotnet/Version.props"
CONDA = ROOT / "bindings/python/conda"
REPOSITORY = "https://github.com/115dkk/vst-audio"
# Submitted next to recipe.yaml; ci-pinning.yaml stands in for conda-forge's
# global pinning in this repository's CI only.
SUBMITTED = ("build.py", "conda_build_config.yaml")


def python_version():
    return tomllib.loads(PYPROJECT.read_text(encoding="utf-8"))["project"]["version"]


def dotnet_version():
    found = re.search(r"<VstAudioVersion>\s*([^<\s]+)\s*</VstAudioVersion>",
                      DOTNET_PROPS.read_text(encoding="utf-8"))
    if not found:
        raise ValueError(f"no VstAudioVersion in {DOTNET_PROPS}")
    return found.group(1)


def version(binding):
    return {"python": python_version, "dotnet": dotnet_version}[binding]()


def recipe_version(text):
    found = re.search(r'^context:\n(?:  .*\n)*?  version: "([^"]+)"', text, re.MULTILINE)
    if not found:
        raise ValueError("recipe.yaml has no context.version")
    return found.group(1)


def registry_url(binding, number):
    if binding == "python":
        return f"https://pypi.org/pypi/vst-audio/{number}/json"
    return "https://api.nuget.org/v3-flatcontainer/vstaudio/index.json"


def published(binding, number, fetch=None):
    """Whether the registry already has this version (a 404 means nobody published it)."""
    def default_fetch(url):
        with urllib.request.urlopen(url, timeout=30) as response:
            return response.read()
    fetch = fetch or default_fetch
    try:
        body = fetch(registry_url(binding, number))
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return False
        raise
    if binding == "python":
        return True
    return number.lower() in json.loads(body)["versions"]


def staged_recipe(text, sha256):
    """The in-repository recipe with its path source replaced by the tag archive."""
    if not re.fullmatch(r"[0-9a-f]{64}", sha256):
        raise ValueError("sha256 must be 64 lowercase hex digits")
    lines = text.splitlines(keepends=True)
    start = next(i for i, line in enumerate(lines) if line == "source:\n")
    end = next(i for i in range(start + 1, len(lines)) if re.match(r"[a-z_]+:", lines[i]))
    source = (
        "source:\n"
        f"  url: {REPOSITORY}/archive/refs/tags/python-v${{{{ version }}}}.tar.gz\n"
        f"  sha256: {sha256}\n"
        "\n"
    )
    return "".join(lines[:start]) + source + "".join(lines[end:])


def archive_sha256(number):
    url = f"{REPOSITORY}/archive/refs/tags/python-v{number}.tar.gz"
    digest = hashlib.sha256()
    with urllib.request.urlopen(url, timeout=120) as response:
        for chunk in iter(lambda: response.read(1 << 16), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_staged_recipe(out, sha256=None):
    text = (CONDA / "recipe.yaml").read_text(encoding="utf-8")
    number = recipe_version(text)
    if number != python_version():
        raise ValueError(f"recipe.yaml is {number} but pyproject.toml is {python_version()}")
    folder = Path(out) / "recipes" / "vst-audio"
    folder.mkdir(parents=True, exist_ok=True)
    (folder / "recipe.yaml").write_text(staged_recipe(text, sha256 or archive_sha256(number)),
                                        encoding="utf-8")
    for name in SUBMITTED:
        shutil.copyfile(CONDA / name, folder / name)
    return folder


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("version", "published"):
        command = commands.add_parser(name)
        command.add_argument("binding", choices=("python", "dotnet"))
    staged = commands.add_parser("staged-recipe")
    staged.add_argument("--out", type=Path, required=True)
    staged.add_argument("--sha256", help="archive digest; downloaded and hashed when omitted")
    args = parser.parse_args(argv)
    if args.command == "version":
        print(version(args.binding))
    elif args.command == "published":
        print("true" if published(args.binding, version(args.binding)) else "false")
    else:
        print(write_staged_recipe(args.out, args.sha256))


if __name__ == "__main__":
    sys.exit(main())
