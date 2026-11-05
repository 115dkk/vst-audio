"""Build fixtures and test the packed NuGet package in a fresh cache (Python 3.11+)."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import uuid

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "tools"))
from binding_release import dotnet_version
from fixtures import advanced_paths, prepare
from stage_native import stage_native

VERSION = dotnet_version()


def run(*args, env=None):
    subprocess.run(list(map(str, args)), cwd=ROOT, env=env, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-dir", type=Path, help="Test an existing packed VstAudio package")
    parser.add_argument("--filter", help="Forward a test filter to dotnet test")
    arguments = parser.parse_args()
    package_dir = arguments.package_dir
    worker, vst2, vst3 = prepare()
    if package_dir is None:
        stage_native()
        package_dir = ROOT / "target/dotnet-pack"
        run("dotnet", "pack", HERE / "VstAudio/VstAudio.csproj", "--configuration", "Release",
            "--output", package_dir)
    package_dir = package_dir.resolve()
    if not (package_dir / f"VstAudio.{VERSION}.nupkg").is_file():
        raise FileNotFoundError(f"VstAudio.{VERSION}.nupkg missing in {package_dir}")
    advanced_vst2, advanced_vst3 = advanced_paths()
    env = dict(os.environ, VST_AUDIO_WORKER=str(worker.resolve()),
               VST_AUDIO_TEST_VST2=str(vst2.resolve()), VST_AUDIO_TEST_VST3=str(vst3.resolve()),
               VST_AUDIO_ADVANCED_VST2=str(advanced_vst2.resolve()),
               VST_AUDIO_ADVANCED_VST3=str(advanced_vst3.resolve()))
    # A unique packages path forces restore to read this exact repacked package.
    packages = ROOT / "target/dotnet-packages" / uuid.uuid4().hex
    command = ["dotnet", "test", HERE / "VstAudio.Tests", f"-p:VstAudioPackageDir={package_dir}",
               f"-p:RestorePackagesPath={packages}", "--logger", "console;verbosity=normal"]
    if arguments.filter:
        command.extend(["--filter", arguments.filter])
    run(*command, env=env)
    # Earlier VstAudio versions are on nuget.org; the tests must have run against the package under test.
    metadata = json.loads((packages / f"vstaudio/{VERSION.lower()}/.nupkg.metadata").read_text(encoding="utf-8"))
    if Path(metadata["source"]).resolve() != package_dir:
        raise SystemExit(f"restored VstAudio from {metadata['source']}, not {package_dir}")


if __name__ == "__main__":
    main()
