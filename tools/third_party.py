"""Build pinned independent plugins outside the checkout, then assert semantics.

The CI artifact allowlist is only the JSON report and bounded text log. Neither
the source tree nor any plugin executable is put in Cargo's cache or artifacts.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
from fixtures import download, target_directory

ROOT = Path(__file__).resolve().parents[1]
PINS = {
    "dpf": ("https://github.com/DISTRHO/DPF.git", "b801214e9ce41b56907e1c6c84859b6b4536917d"),
    "dpf-plugins": ("https://github.com/DISTRHO/DPF-Plugins.git", "df5cb658a2d8dc6e944d8c26a53853a7c0c6c2b2"),
    "sdk": ("https://github.com/steinbergmedia/vst3sdk.git", "3cdf9ca5d1f5b1b21e0a86832aa4abe55607bd96"),
}

# EqualizerAPO-XT release used by the engine comparison. GPL binaries stay
# outside the repository, Cargo caches and CI artifacts.
EAPO = ("https://github.com/115dkk/equalizerapo-xt/releases/download/v2.60.6/EqualizerAPO-XT-x64-sse2-2.60.6-x64-sse2-full.nupkg",
        "b76a04db00f0513746c00e1ac2c6ae036f6524e03bbf6a134f1432bdd977c7db")

# A GPU-less Windows runner has only the GDI OpenGL 1.1, on which DPF editors draw
# nothing (their pixels read near black). As with Mesa under Xvfb on Linux, a pinned
# Mesa llvmpipe renders them in software. The worker loads plugins with
# LOAD_WITH_ALTERED_SEARCH_PATH, which resolves their imports from the plugin's own
# folder before System32 and never from the worker's, so the DLLs go beside each
# plugin binary in the external build tree.
MESA = ("https://github.com/pal1000/mesa-dist-win/releases/download/26.2.4/mesa3d-26.2.4-release-msvc.7z",
        "351fc8c8b695878ffb3eaa044b3ead08672a48b1a045e3c3e3975811df0f6695")

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
    if name == "dpf":
        run("git", "submodule", "update", "--init", "--depth", "1", "dgl/src/pugl-upstream", cwd=directory)
    if name == "sdk":
        run("git", "submodule", "update", "--init", "--depth", "1", "base", "cmake", "pluginterfaces", "public.sdk", cwd=directory)
    assert not subprocess.check_output(["git", "status", "--porcelain"], cwd=directory).strip(), "modified upstream source"
    return directory

def build(root):
    dpf, plugins, sdk = (checkout(root, name) for name in PINS)
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
dpf_add_plugin(d_parameters TARGETS vst2 vst3 FILES_DSP "{dpf.as_posix()}/examples/Parameters/ExamplePluginParameters.cpp" FILES_UI "{dpf.as_posix()}/examples/Parameters/ExampleUIParameters.cpp")
target_include_directories(d_parameters PUBLIC "{dpf.as_posix()}/examples/Parameters")
dpf_add_plugin(d_sendNote TARGETS vst2 vst3 FILES_DSP "{dpf.as_posix()}/examples/SendNote/SendNoteExamplePlugin.cpp" FILES_UI "{dpf.as_posix()}/examples/SendNote/SendNoteExampleUI.cpp")
target_include_directories(d_sendNote PUBLIC "{dpf.as_posix()}/examples/SendNote")
dpf_add_plugin(d_3BandEQ TARGETS vst2 vst3 FILES_DSP "{plugins.as_posix()}/plugins/3BandEQ/DistrhoPlugin3BandEQ.cpp" FILES_UI "{plugins.as_posix()}/plugins/3BandEQ/DistrhoArtwork3BandEQ.cpp" "{plugins.as_posix()}/plugins/3BandEQ/DistrhoUI3BandEQ.cpp")
target_include_directories(d_3BandEQ PUBLIC "{plugins.as_posix()}/plugins/3BandEQ")
dpf_add_plugin(d_MVerb TARGETS vst2 vst3 FILES_DSP "{plugins.as_posix()}/plugins/MVerb/DistrhoPluginMVerb.cpp" FILES_UI "{plugins.as_posix()}/plugins/MVerb/DistrhoArtworkMVerb.cpp" "{plugins.as_posix()}/plugins/MVerb/DistrhoUIMVerb.cpp")
target_include_directories(d_MVerb PUBLIC "{plugins.as_posix()}/plugins/MVerb")
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
    for case, name in [("MIDI", "d_midiThrough"), ("LATENCY", "d_latency"), ("PARAMETERS", "d_parameters"), ("SENDNOTE", "d_sendNote")]:
        paths[f"{case}_VST3"] = dpf_build / "bin" / f"{name}.vst3"
        paths[f"{case}_VST2"] = dpf_build / "bin" / (f"{name}.vst" if sys.platform == "darwin" else f"{name}-vst2" + (".dll" if sys.platform == "win32" else ".so"))
    paths["EQ_VST3"] = dpf_build / "bin/d_3BandEQ.vst3"
    paths["EQ_VST2"] = dpf_build / "bin" / ("d_3BandEQ.vst" if sys.platform == "darwin" else "d_3BandEQ-vst2" + (".dll" if sys.platform == "win32" else ".so"))
    paths["MVERB_VST3"] = dpf_build / "bin/d_MVerb.vst3"
    paths["MVERB_VST2"] = dpf_build / "bin" / ("d_MVerb.vst" if sys.platform == "darwin" else "d_MVerb-vst2" + (".dll" if sys.platform == "win32" else ".so"))
    for case, name in [("DELAY", "adelay"), ("MDA", "mda-vst3")]:
        candidates = [p for p in sdk_build.rglob(f"{name}.vst3") if p.is_dir()]
        assert len(candidates) == 1, candidates
        paths[f"{case}_VST3"] = candidates[0]
    for path in paths.values():
        assert path.exists(), path
        if sys.platform == "darwin":
            run("codesign", "--force", "--sign", "-", path)
    return paths

def install_software_opengl(root, plugins):
    """Place Mesa's WGL DLLs beside every plugin binary built outside the checkout."""
    url, digest = MESA
    archive, extracted = root / "mesa.7z", root / "mesa"
    if not archive.exists() or hashlib.sha256(archive.read_bytes()).hexdigest() != digest:
        archive.write_bytes(download(url, timeout=120))
    actual = hashlib.sha256(archive.read_bytes()).hexdigest()
    assert actual == digest, f"Mesa archive digest {actual}"
    if not (extracted / "x64" / "opengl32.dll").exists():
        run("7z", "x", "-y", f"-o{extracted}", archive, "x64/opengl32.dll", "x64/libgallium_wgl.dll")
    directories = set()
    for plugin in plugins:
        binaries = list((plugin / "Contents" / "x86_64-win").glob("*.vst3")) if plugin.is_dir() else [plugin]
        assert binaries, plugin
        directories.update(binary.parent for binary in binaries)
    for directory in sorted(directories):
        for dll in ("opengl32.dll", "libgallium_wgl.dll"):
            shutil.copy2(extracted / "x64" / dll, directory / dll)
    return sorted(str(directory) for directory in directories)

def install_eapo(root) -> Path:
    """Verify and unpack the black-box GPL engine only in the external work tree."""
    import zipfile
    url, digest = EAPO
    folder = root / "eapo-package"
    folder.mkdir(exist_ok=True)
    package = folder / "release.nupkg"
    if not package.exists() or hashlib.sha256(package.read_bytes()).hexdigest() != digest:
        package.write_bytes(download(url, timeout=120))
    actual = hashlib.sha256(package.read_bytes()).hexdigest()
    assert actual == digest, f"EqualizerAPO-XT archive digest {actual}"
    app = root / "eapo"
    # Re-extract verified bytes: a reused directory must not silently use stale binaries.
    with zipfile.ZipFile(package) as archive:
        for member in archive.infolist():
            if member.filename.startswith("lib/app/") and not member.is_dir():
                target = app / member.filename[len("lib/app/"):]
                assert app.resolve() in target.resolve().parents
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(archive.read(member))
    assert (app / "Editor.exe").is_file() and (app / "Benchmark.exe").is_file()
    return app

def linux_vst2_hidpi(root, worker, plugins, env):
    """Open VST2 editors at two Xft DPI values through the public crate API.

    `plugins` is a list of (label, path); each editor must double its size at
    192 DPI because the host's content scale request was accepted.
    """
    consumer = root / "hidpi-consumer"
    (consumer / "src").mkdir(parents=True, exist_ok=True)
    (consumer / "Cargo.toml").write_text(f'''[package]
name = "vst2-hidpi-consumer"
version = "0.0.0"
edition = "2024"

[dependencies]
vst-audio = {{ path = {json.dumps((ROOT / "crates/vst-audio").as_posix())} }}
x11rb = "0.13"
''', encoding="utf-8")
    (consumer / "src/main.rs").write_text(r'''
#![forbid(unsafe_code)]
use std::{path::PathBuf, time::{Duration, Instant}};
use vst_audio::{Command, Configuration, EditorScaling, Host, HostEvent, PluginDescriptor, PluginFormat};
use x11rb::{connection::Connection, protocol::xproto::*, rust_connection::RustConnection, wrapper::ConnectionExt as _};
struct ResourceManager { connection: RustConnection, root: u32, atom: u32, previous: Option<(u32, u8, Vec<u8>)> }
impl ResourceManager {
    fn set(dpi: u32) -> Self {
        let (connection, screen) = x11rb::connect(None).unwrap();
        let root = connection.setup().roots[screen].root;
        let atom = connection.intern_atom(false, b"RESOURCE_MANAGER").unwrap().reply().unwrap().atom;
        let reply = connection.get_property(false, root, atom, AtomEnum::ANY, 0, u32::MAX).unwrap().reply().unwrap();
        let previous = (reply.type_ != 0).then_some((reply.type_, reply.format, reply.value));
        connection.change_property8(PropMode::REPLACE, root, atom, AtomEnum::STRING,
            format!("Xft.dpi: {dpi}\n").as_bytes()).unwrap().check().unwrap();
        connection.flush().unwrap();
        Self { connection, root, atom, previous }
    }
}
impl Drop for ResourceManager {
    fn drop(&mut self) {
        let result = if let Some((type_, format, value)) = &self.previous {
            self.connection.change_property(PropMode::REPLACE, self.root, self.atom, *type_, *format,
                u32::try_from(value.len() / (usize::from(*format) / 8)).unwrap(), value)
        } else { self.connection.delete_property(self.root, self.atom) };
        if let Ok(cookie) = result { let _ = cookie.check(); }
        let _ = self.connection.flush();
    }
}
fn open(worker: &PathBuf, plugin: &PathBuf, dpi: u32) -> (f32, u32, u32) {
    let _resource = ResourceManager::set(dpi);
    let host = Host::new(worker.clone());
    let descriptor = PluginDescriptor::new(plugin.clone(), PluginFormat::Vst2);
    let instance = host.spawn(&descriptor, &Configuration::default(), None).unwrap();
    let request = instance.controller().request(Command::OpenEditorWith {
        parent: None, scaling: EditorScaling::Auto,
    }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(event) = instance.try_next_event() {
            match event {
                HostEvent::Completed { request: id } if id == request => break,
                HostEvent::RequestFailed { request: id, reason } if id == request => panic!("{reason}"),
                HostEvent::Failed { reason } => panic!("{reason}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "editor open timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    let request = instance.controller().request(Command::Editor(vst_audio::EditorAction::Inspect)).unwrap();
    loop {
        if let Some(HostEvent::EditorStatus { request: id, status }) = instance.try_next_event()
            && id == request {
                assert_eq!(status.scaling, EditorScaling::Plugin);
                return (status.scale, status.width, status.height);
            }
        assert!(Instant::now() < deadline, "editor status timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn main() {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let _display = x11rb::connect(None).unwrap().0;
    for pair in args[1..].chunks(2) {
        let (label, plugin) = (pair[0].display(), &pair[1]);
        let base = open(&args[0], plugin, 96);
        let high = open(&args[0], plugin, 192);
        assert_eq!(base.0, 1.0, "{label}");
        assert_eq!(high.0, 2.0, "{label}");
        assert_eq!((high.1, high.2), (base.1 * 2, base.2 * 2), "{label}");
        println!("{label}: 96dpi={base:?}, 192dpi={high:?}");
    }
}
''', encoding="utf-8")
    result = subprocess.run(["cargo", "run", "--manifest-path", str(consumer / "Cargo.toml"),
        "--target-dir", str(Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "third-party-consumer"),
        "--", str(worker), *(str(item) for pair in plugins for item in pair)],
        cwd=ROOT, env=env, capture_output=True, text=True, timeout=300)
    output = result.stdout + result.stderr
    print(output[-32768:], flush=True)
    assert result.returncode == 0, "VST2 HiDPI public consumer failed"
    return result.stdout.strip().splitlines()[-len(plugins):]

# JUCE VST2 plugins from the distribution (the runner installs iem-plugin-suite-vst).
# SimpleDecoder loads libX11 only when its editor opens and installs no X error
# handler; StereoEncoder declares 64 outputs and reports a narrower current layout.
IEM = Path("/usr/lib/x86_64-linux-gnu/iem-plugin-suite/vst")

def iem_package():
    fields = subprocess.run(["dpkg-query", "-W", "-f", "${Version}\t${Built-Using}", "iem-plugin-suite-vst"],
        check=True, capture_output=True, text=True).stdout.split("\t")
    assert "juce" in fields[1], "iem-plugin-suite-vst is expected to be built with JUCE"
    return {"package": "iem-plugin-suite-vst", "version": fields[0], "built_using": fields[1]}


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
        target = target_directory()
        worker = target / "debug" / ("vst-audio-worker.exe" if sys.platform == "win32" else "vst-audio-worker")
        if sys.platform == "win32":
            installed = install_software_opengl(root, paths.values())
            report["software_opengl"] = {"url": MESA[0], "sha256": MESA[1], "driver": "llvmpipe",
                                         "directories": len(installed)}
        env = dict(os.environ, VST_AUDIO_WORKER=str(worker), **{f"VST_AUDIO_REAL_{key}": str(value) for key, value in paths.items()})
        if sys.platform.startswith("linux"):
            report["iem"] = iem_package()
            report["vst2_hidpi"] = linux_vst2_hidpi(root, worker, [
                ("DPF d_parameters VST2", paths["PARAMETERS_VST2"]),
                ("JUCE IEM SimpleDecoder VST2", IEM / "libSimpleDecoder.so"),
                ("JUCE IEM StereoEncoder VST2", IEM / "libStereoEncoder.so"),
            ], env)
        if sys.platform == "win32":
            env["GALLIUM_DRIVER"] = "llvmpipe"
            app = install_eapo(root)
            # EQ APO XT loads plugins with the default search order, which starts
            # at its own folder: Mesa there renders the DPF editors in its panel.
            mesa = root / "mesa" / "x64"
            for dll in ("opengl32.dll", "libgallium_wgl.dll"):
                shutil.copy2(mesa / dll, app / dll)
            env["VST_AUDIO_EAPO"] = str(app)
            work = root / "eapo-work"
            work.mkdir(exist_ok=True)
            assert not any(work.iterdir()), "EqualizerAPO comparison needs an empty eapo-work directory"
            env["VST_AUDIO_EAPO_WORK"] = str(work)
            report["equalizer_apo_xt"] = {"url": EAPO[0], "sha256": EAPO[1]}
        # Core dumps can contain native code pages. They are neither needed nor retained.
        if sys.platform != "win32":
            import resource
            resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        result = subprocess.run(["cargo", "test", "--locked", "-p", "vst-audio", "--test", "third_party", "--", "--ignored", "--nocapture", "--test-threads=1"], cwd=ROOT, env=env, capture_output=True, text=True, timeout=1500)
        output = result.stdout + result.stderr
        print(output[-131072:], flush=True)
        (report_dir / "checks.log").write_text(output[-131072:], encoding="utf-8")
        report["metrics"] = [json.loads(line.partition("METRIC ")[2]) for line in output.splitlines() if "METRIC " in line]
        report["returncode"] = result.returncode
        assert len(report["metrics"]) >= 10, "semantic test measurements missing"
        report["known_upstream_limitations"]=[m["case"] for m in report["metrics"] if m["case"].startswith("known limitation:")]
        if sys.platform != "darwin":
            mutant_target = root / "mutation-target"
            run("cargo", "build", "--locked", "-p", "vst-audio-worker", "--features",
                "vst-audio-worker-sys/__test-drop-editor-edits", "--target-dir", mutant_target)
            mutant = mutant_target / "debug" / worker.name
            mutation_env = dict(env, VST_AUDIO_WORKER=str(mutant))
            result = subprocess.run(["cargo", "test", "--locked", "-p", "vst-audio", "--test", "third_party",
                "editor_input_eq_vst3", "--", "--exact", "--ignored", "--nocapture", "--test-threads=1"],
                cwd=ROOT, env=mutation_env, capture_output=True, text=True, timeout=120)
            output = result.stdout + result.stderr
            print(output[-131072:], flush=True)
            (report_dir / "mutation.log").write_text(output[-131072:], encoding="utf-8")
            marker = "editor-to-processor sound assertion failed"
            mutation_passed = result.returncode != 0 and marker in output
            report["mutation"] = {"returncode": result.returncode, "sound_failure": marker in output,
                "passed": mutation_passed,
                "metrics": [json.loads(line.partition("METRIC ")[2]) for line in output.splitlines() if "METRIC " in line]}
            assert mutation_passed, "mutation did not fail at the sound assertion"
        else:
            report["mutation"] = {"not_run": "macOS synthetic editor input is unsupported"}
        if report["returncode"]:
            raise SystemExit(report["returncode"])
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
