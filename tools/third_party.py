"""Build pinned independent plugins outside the checkout, then assert semantics.

The CI artifact allowlist is only the JSON report and bounded text log. Neither
the source tree nor any plugin executable is put in Cargo's cache or artifacts.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PINS = {
    "dpf": ("https://github.com/DISTRHO/DPF.git", "4238e1c7f0351bbe488d79f0899c540543ac7583"),
    "sdk": ("https://github.com/steinbergmedia/vst3sdk.git", "3cdf9ca5d1f5b1b21e0a86832aa4abe55607bd96"),
}

def run(*command, cwd=ROOT):
    subprocess.run(list(map(str, command)), cwd=cwd, check=True)

def checkout(root, name):
    url, revision = PINS[name]
    directory = root / name
    if not directory.exists():
        run("git", "clone", "--filter=blob:none", "--no-checkout", url, directory)
    run("git", "checkout", "--detach", revision, cwd=directory)
    actual = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=directory, text=True).strip()
    assert actual == revision
    assert not subprocess.check_output(["git", "status", "--porcelain"], cwd=directory).strip(), "modified upstream source"
    if name == "sdk":
        run("git", "submodule", "update", "--init", "--depth", "1", "base", "cmake", "pluginterfaces", "public.sdk", cwd=directory)
    return directory

def build(root):
    dpf, sdk = (checkout(root, name) for name in PINS)
    wrapper = root / "dpf-project"
    wrapper.mkdir(exist_ok=True)
    (wrapper / "CMakeLists.txt").write_text(f'''cmake_minimum_required(VERSION 3.25)
project(vst_audio_independent_plugins LANGUAGES C CXX)
set(DPF_LIBRARIES OFF CACHE BOOL "" FORCE)
set(DPF_EXAMPLES OFF CACHE BOOL "" FORCE)
add_subdirectory("{dpf.as_posix()}" dpf)
dpf_add_plugin(d_midiThrough TARGETS vst2 vst3 FILES_DSP "{dpf.as_posix()}/examples/MidiThrough/MidiThroughExamplePlugin.cpp")
target_include_directories(d_midiThrough PUBLIC "{dpf.as_posix()}/examples/MidiThrough")
dpf_add_plugin(d_latency TARGETS vst2 vst3 FILES_DSP "{dpf.as_posix()}/examples/Latency/LatencyExamplePlugin.cpp")
target_include_directories(d_latency PUBLIC "{dpf.as_posix()}/examples/Latency")
''', encoding="utf-8")
    dpf_build, sdk_build = root / "dpf-build", root / "sdk-build"
    run("cmake", "-S", wrapper, "-B", dpf_build, "-DCMAKE_BUILD_TYPE=Release")
    run("cmake", "--build", dpf_build, "--config", "Release", "--parallel", "4")
    run("cmake", "-S", sdk, "-B", sdk_build, "-DCMAKE_BUILD_TYPE=Release",
        "-DSMTG_ENABLE_VSTGUI_SUPPORT=OFF", "-DSMTG_ENABLE_VST3_HOSTING_EXAMPLES=OFF",
        "-DSMTG_CREATE_PLUGIN_LINK=OFF", "-DSMTG_RUN_VST_VALIDATOR=OFF",
        "-DSMTG_CREATE_MODULE_INFO=OFF")
    run("cmake", "--build", sdk_build, "--config", "Release", "--parallel", "4", "--target", "adelay", "mda-vst3")
    paths = {}
    for case, name in [("MIDI", "d_midiThrough"), ("LATENCY", "d_latency")]:
        paths[f"{case}_VST3"] = dpf_build / "bin" / f"{name}.vst3"
        paths[f"{case}_VST2"] = dpf_build / "bin" / (f"{name}.vst" if sys.platform == "darwin" else f"{name}-vst2" + (".dll" if sys.platform == "win32" else ".so"))
    for case, name in [("DELAY", "adelay"), ("MDA", "mda-vst3")]:
        candidates = [p for p in sdk_build.rglob(f"{name}.vst3") if p.is_dir()]
        assert len(candidates) == 1, candidates
        paths[f"{case}_VST3"] = candidates[0]
    for path in paths.values():
        assert path.exists(), path
        if sys.platform == "darwin":
            run("codesign", "--force", "--sign", "-", path)
    return paths

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, help="Reuse a developer-owned directory outside the checkout")
    parser.add_argument("--build-only", action="store_true")
    options = parser.parse_args()
    temporary = options.root is None
    root = (options.root or Path(tempfile.mkdtemp(prefix="vst-audio-real-", dir=os.environ.get("RUNNER_TEMP")))).resolve()
    assert root != ROOT and ROOT not in root.parents, "external plugins must stay outside the checkout/Cargo cache"
    root.mkdir(parents=True, exist_ok=True)
    report_dir = ROOT / "test-results/third-party"
    report_dir.mkdir(parents=True, exist_ok=True)
    report = {"sources": {name: {"url": url, "commit": commit} for name, (url, commit) in PINS.items()}, "platform": sys.platform, "passed": False, "metrics": []}
    try:
        paths = build(root)
        (root / "paths.json").write_text(json.dumps({key: str(value) for key, value in paths.items()}, indent=2))
        if options.build_only:
            return
        run("cargo", "build", "--locked", "-p", "vst-audio-worker")
        target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
        worker = target / "debug" / ("vst-audio-worker.exe" if sys.platform == "win32" else "vst-audio-worker")
        env = dict(os.environ, VST_AUDIO_WORKER=str(worker), **{f"VST_AUDIO_REAL_{key}": str(value) for key, value in paths.items()})
        # Core dumps can contain native code pages. They are neither needed nor retained.
        if sys.platform != "win32":
            import resource
            resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        result = subprocess.run(["cargo", "test", "--locked", "-p", "vst-audio", "--test", "third_party", "--", "--ignored", "--nocapture", "--test-threads=1"], cwd=ROOT, env=env, capture_output=True, text=True, timeout=240)
        output = result.stdout + result.stderr
        print(output[-131072:], flush=True)
        (report_dir / "checks.log").write_text(output[-131072:], encoding="utf-8")
        report["metrics"] = [json.loads(line.partition("METRIC ")[2]) for line in output.splitlines() if "METRIC " in line]
        if result.returncode:
            raise SystemExit(result.returncode)
        assert len(report["metrics"]) >= 10, "semantic test measurements missing"
        report["known_upstream_limitations"]=[m["case"] for m in report["metrics"] if m["case"].startswith("known limitation:")]
        report["passed"] = True
    finally:
        (report_dir / "summary.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
        if temporary:
            # Delete only this exact mkdtemp-created directory, outside the checkout.
            parent = Path(os.environ.get("RUNNER_TEMP") or tempfile.gettempdir()).resolve()
            assert root.parent == parent and root.name.startswith("vst-audio-real-") and not root.is_symlink()
            def remove_readonly(function, path, _error):
                resolved=Path(path).resolve()
                assert root in resolved.parents, "cleanup escaped its temporary tree"
                os.chmod(resolved,stat.S_IWRITE|stat.S_IREAD)
                function(path)
            shutil.rmtree(root,onerror=remove_readonly)

if __name__ == "__main__":
    main()
