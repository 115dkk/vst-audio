"""Build a fresh consumer from normalized .crate archives, without publishing.

Local registry patches point only to extracted archives, not the source checkout.
Packaging runs in a copied workspace so Cargo cannot mutate the developer lock.
"""
from pathlib import Path
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

from fixtures import ROOT, prepare

worker, vst2_plugin, plugin = prepare()
version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
names = [
    "vst-audio-ring", "vst-audio-protocol", "vst-audio-transport-sys",
    "vst-audio", "vst-audio-worker-sys", "vst-audio-worker-runtime", "vst-audio-worker",
]
temp_parent = Path(tempfile.gettempdir()).resolve()
scratch = Path(tempfile.mkdtemp(prefix="vst-audio-package-", dir=temp_parent)).resolve()
artifact = ROOT / "target" / "package-check"
artifact.mkdir(parents=True, exist_ok=True)
stage = "copy"
summary = {"status": "failed", "packages": names, "version": version}

def run(command, cwd):
    result = subprocess.run(command, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (artifact / (stage.replace(" ", "-") + ".log")).write_text(result.stdout, encoding="utf-8")
    if result.returncode:
        print(result.stdout[-16000:])
        raise subprocess.CalledProcessError(result.returncode, command)
    print(f"{stage}: passed", flush=True)

try:
    shutil.copyfile(ROOT / "rust-toolchain.toml", scratch / "rust-toolchain.toml")
    source = scratch / "source"
    source.mkdir()
    for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "README.md", "LICENSE", "NOTICE"]:
        shutil.copyfile(ROOT / name, source / name)
    for directory in ["crates", "apps"]:
        shutil.copytree(ROOT / directory, source / directory)
    vendor = scratch / "archives"
    vendor.mkdir()
    patches = []
    package_target = scratch / "package-target"
    for name in names:
        stage = f"package {name}"
        command = ["cargo", "package", "-p", name, "--no-verify",
                   "--allow-dirty", "--target-dir", str(package_target)]
        for package, path in patches:
            command += ["--config", f"patch.crates-io.{package}.path={json.dumps(path.as_posix())}"]
        run(command, source)
        archive = package_target / "package" / f"{name}-{version}.crate"
        with tarfile.open(archive, "r:gz") as contents:
            contents.extractall(vendor, filter="data")
        package_root = vendor / f"{name}-{version}"
        if not (package_root / "LICENSE").is_file():
            raise RuntimeError(f"{name} package omitted its license")
        patches.append((name, package_root))
        shutil.copyfile(archive, artifact / archive.name)

    stage = "packaged worker"
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "package-consumer"
    command = ["cargo", "build", "--manifest-path",
               str(vendor / f"vst-audio-worker-{version}" / "Cargo.toml"),
               "--target-dir", str(target)]
    for package, path in patches:
        command += ["--config", f"patch.crates-io.{package}.path={json.dumps(path.as_posix())}"]
    run(command, vendor)
    worker = target / "debug" / ("vst-audio-worker.exe" if os.name == "nt" else "vst-audio-worker")
    stage = "fresh consumer"
    consumer = scratch / "consumer"
    (consumer / "src").mkdir(parents=True)
    manifest = (
        '[package]\nname="package-consumer"\nversion="0.0.0"\nedition="2024"\n'
        f'[dependencies]\nvst-audio="={version}"\n[patch.crates-io]\n'
        + "".join(f'{name}={{path={json.dumps(path.as_posix())}}}\n' for name, path in patches)
    )
    (consumer / "Cargo.toml").write_text(manifest, encoding="utf-8")
    (consumer / "src/main.rs").write_text(r'''
#![forbid(unsafe_code)]
use std::{path::PathBuf, time::{Duration, Instant}};
use vst_audio::{AudioStatus, Configuration, Host, PluginDescriptor, PluginFormat};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let host = Host::new(PathBuf::from(&args[0]));
    exercise(&host, PathBuf::from(&args[1]), PluginFormat::Vst2)?;
    exercise(&host, PathBuf::from(&args[2]), PluginFormat::Vst3)?;
    Ok(())
}
fn exercise(host: &Host, path: PathBuf, format: PluginFormat) -> Result<(), Box<dyn std::error::Error>> {
    let plugin = PluginDescriptor::new(path, format);
    assert!(!host.scan(&plugin)?.is_empty());
    let mut instance = host.spawn(&plugin, &Configuration::default(), None)?;
    let mut audio = instance.take_audio().unwrap();
    let geometry = audio.geometry();
    let inputs = vec![vec![0.25; geometry.max_frames as usize]; geometry.input_channels as usize];
    let mut outputs = vec![vec![0.0; geometry.max_frames as usize]; geometry.output_channels as usize];
    let inputs: Vec<_> = inputs.iter().map(Vec::as_slice).collect();
    let mut outputs: Vec<_> = outputs.iter_mut().map(Vec::as_mut_slice).collect();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if audio.process(&inputs, &mut outputs) == AudioStatus::Processed { break; }
        assert!(Instant::now() < deadline, "packaged consumer did not process");
        std::thread::sleep(Duration::from_millis(10));
    }
    println!("Fresh packaged consumer processed a {format:?} block.");
    Ok(())
}
''', encoding="utf-8")
    # A stable build cache, not a source dependency; all source comes from archives.
    run(["cargo", "run", "--manifest-path", str(consumer / "Cargo.toml"),
         "--target-dir", str(target), "--", str(worker), str(vst2_plugin), str(plugin)], consumer)
    stage = "done"
    summary["status"] = "passed"
finally:
    summary["stage"] = stage
    (artifact / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    # Validate the exact temporary tree before recursive cleanup.
    resolved = scratch.resolve()
    if resolved.parent != temp_parent or not resolved.name.startswith("vst-audio-package-"):
        raise RuntimeError("unexpected temporary packaging directory")
    shutil.rmtree(resolved)
