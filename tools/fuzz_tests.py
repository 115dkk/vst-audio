"""Bounded coverage-guided runs; retains only the public-input corpus/logs."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

from run import to_log

ROOT = Path(__file__).resolve().parents[1]
REPORT = ROOT / "test-results/fuzz"
CORPUS = ROOT / "fuzz/corpus/control"
GENERATED_PATTERN = "known-??-*.bin"
LEGACY_GENERATED_SEEDS = {"hello", "ack", "parameters", "image", "state"}

RUST_CONSUMER = r'''
#![forbid(unsafe_code)]
use std::{env, fs, path::Path};
use vst_audio_protocol::{
    Capabilities, DecodedFrame, EditPhase, EditorAction, EditorImage, EditorPlatform,
    EditorScaling, EditorStatus, Frame, Message, ParentWindow, PluginFormat, ProcessingSetup,
    SampleFormat, decode_frame, encode_frame,
};

fn messages() -> Vec<(&'static str, Message)> {
    vec![
        ("hello", Message::hello()),
        ("load", Message::Load { path: "plugin.vst3".into(), format: PluginFormat::Vst3, class_id: None }),
        ("configure", Message::Configure { rate: 48_000, max_frames: 512, in_channels: 2, out_channels: 2 }),
        ("activate", Message::Activate),
        ("set-state", Message::SetState { blob: b"state".to_vec() }),
        ("get-state", Message::GetState),
        ("open-editor", Message::OpenEditor { scaling: EditorScaling::Auto }),
        ("shutdown", Message::Shutdown),
        ("scan", Message::Scan { path: "plugin.vst3".into() }),
        ("loaded", Message::Loaded { name: "plugin".into(), vendor: "vendor".into(), latency_frames: 0, inputs: vec![], outputs: vec![] }),
        ("classes", Message::Classes { entries: vec![] }),
        ("latency-changed", Message::LatencyChanged { frames: 64 }),
        ("state", Message::State { blob: b"state".to_vec() }),
        ("params-changed", Message::ParamsChanged),
        ("fault", Message::Fault { code: 1, text: "fault".into() }),
        ("ack", Message::Ack),
        ("editor-closed", Message::EditorClosed),
        ("close-editor", Message::CloseEditor),
        ("list-parameters", Message::ListParameters { start: 0, count: 1 }),
        ("parameters", Message::Parameters { total: 0, entries: vec![] }),
        ("set-parameter", Message::SetParameter { id: 1, value: 0.5 }),
        ("request-error", Message::RequestError { code: 1, text: "error".into() }),
        ("prepare", Message::Prepare(ProcessingSetup { sample_rate: 48_000, quantum: 512, sample_format: SampleFormat::F32, offline: false, inputs: vec![], outputs: vec![] })),
        ("describe", Message::Describe),
        ("features", Message::Features(Capabilities::default())),
        ("editor", Message::Editor(EditorAction::Show)),
        ("editor-status", Message::EditorStatus(EditorStatus { platform: EditorPlatform::X11, scaling: EditorScaling::Auto, visible: true, embedded: false, width: 640, height: 480, scale: 1.0, resize_supported: true, scale_supported: true })),
        ("editor-image", Message::EditorImage(EditorImage { width: 1, height: 1, rgba: vec![32, 64, 128, 255] })),
        ("sandboxed", Message::Sandboxed { mechanism: "test".into() }),
        ("open-editor-in", Message::OpenEditorIn { parent: ParentWindow::X11 { window: 1 }, scaling: EditorScaling::Auto }),
        ("editor-resized", Message::EditorResized { width: 640, height: 480 }),
        ("parameter-edit", Message::ParameterEdit { id: 1, phase: EditPhase::Perform, value: 0.5 }),
    ]
}

fn verify(directory: &Path) {
    let mut kinds = Vec::new();
    for entry in fs::read_dir(directory).expect("read corpus").map(Result::unwrap) {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("bin") { continue; }
        let bytes = fs::read(&path).expect("read seed");
        let DecodedFrame::Known(frame) = decode_frame(&bytes).expect("seed must decode") else {
            panic!("{} decoded as Unknown", path.display());
        };
        assert_eq!(encode_frame(&frame).expect("re-encode seed"), bytes, "{}", path.display());
        kinds.push(frame.message.kind());
    }
    kinds.sort_unstable();
    assert_eq!(kinds, (1_u16..=32).collect::<Vec<_>>());
    println!("verified {} Known control seeds (kinds 1..=32)", kinds.len());
}

fn main() {
    let mut args = env::args_os().skip(1);
    let mode = args.next().expect("mode");
    let directory = args.next().expect("directory");
    assert!(args.next().is_none(), "unexpected argument");
    let directory = Path::new(&directory);
    if mode == "generate" {
        fs::create_dir_all(directory).expect("create output");
        let messages = messages();
        assert_eq!(messages.len(), 32);
        for (expected_kind, (name, message)) in (1_u16..=32).zip(messages) {
            assert_eq!(message.kind(), expected_kind, "{name}");
            let bytes = encode_frame(&Frame { flags: 0, request: u32::from(expected_kind), message }).expect("encode seed");
            assert!(matches!(decode_frame(&bytes), Ok(DecodedFrame::Known(_))), "{name}");
            fs::write(directory.join(format!("known-{expected_kind:02}-{name}.bin")), bytes).expect("write seed");
        }
    } else if mode != "verify" {
        panic!("mode must be generate or verify");
    }
    verify(directory);
}
'''


def _consumer(root):
    consumer = root / "consumer"
    (consumer / "src").mkdir(parents=True)
    (consumer / "Cargo.toml").write_text(f'''[package]
name = "vst-audio-control-seeds"
version = "0.0.0"
edition = "2024"

[dependencies]
vst-audio-protocol = {{ path = {json.dumps((ROOT / "crates/vst-audio-protocol").as_posix())} }}

[patch.crates-io]
vst-audio-ring = {{ path = {json.dumps((ROOT / "crates/vst-audio-ring").as_posix())} }}
''', encoding="utf-8")
    (consumer / "src/main.rs").write_text(RUST_CONSUMER, encoding="utf-8")
    return consumer / "Cargo.toml"


def _run_consumer(manifest, mode, directory):
    result = subprocess.run([
        "cargo", "run", "--quiet", "--offline", "--manifest-path", str(manifest),
        "--target-dir", str(ROOT / "target/fuzz-seed-consumer"), "--", mode, str(directory),
    ], cwd=ROOT, capture_output=True, text=True)
    if result.stdout:
        print(result.stdout, end="", flush=True)
    if result.returncode:
        raise RuntimeError(f"control seed {mode} failed:\n{result.stderr[-12000:]}")


def install_generated_seeds(staging, corpus=CORPUS):
    """Replace only seeds owned by this generator, preserving other corpus files."""
    generated = sorted(staging.glob(GENERATED_PATTERN))
    names = [path.name for path in generated]
    if len(generated) != 32 or len(set(names)) != 32:
        raise RuntimeError("control seed generator must produce 32 unique files")
    corpus.mkdir(parents=True, exist_ok=True)
    stale = list(corpus.glob(GENERATED_PATTERN))
    stale.extend(corpus / name for name in LEGACY_GENERATED_SEEDS if (corpus / name).exists())
    for path in stale:
        if path.parent != corpus or not (path.is_file() or path.is_symlink()):
            raise RuntimeError(f"refusing to remove unexpected corpus path: {path}")
        path.unlink()
    for source in generated:
        shutil.copyfile(source, corpus / source.name)
    return names


def generate_control_corpus(corpus=CORPUS):
    temporary_parent = os.environ.get("RUNNER_TEMP")
    with tempfile.TemporaryDirectory(prefix="vst-audio-fuzz-seeds-", dir=temporary_parent) as temporary:
        root = Path(temporary)
        manifest = _consumer(root)
        staging = root / "generated"
        _run_consumer(manifest, "generate", staging)
        _run_consumer(manifest, "verify", staging)
        names = install_generated_seeds(staging, corpus)
        _run_consumer(manifest, "verify", corpus)
    return names


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--generate-control-corpus-only", action="store_true")
    options = parser.parse_args()
    REPORT.mkdir(parents=True, exist_ok=True)
    generated = generate_control_corpus()
    if options.generate_control_corpus_only:
        print(f"installed {len(generated)} control corpus seeds", flush=True)
        return
    seconds = int(os.environ.get("VST_AUDIO_FUZZ_SECONDS", "120"))
    if not 1 <= seconds <= 3600:
        raise ValueError("fuzz duration must be 1..3600 seconds")
    results = []
    try:
        for target in ["control", "ring"]:
            command = ["cargo", "+nightly-2026-10-20", "fuzz", "run", target, "--",
                       "-max_total_time=" + str(seconds), "-max_len=65536",
                       "-rss_limit_mb=2048", "-print_final_stats=1"]
            result = to_log(command, REPORT / (target + ".log"), cwd=ROOT,
                            timeout=seconds + 300, tail_bytes=12000)
            print(result.tail, flush=True)
            units = re.search(r"stat::number_of_executed_units:\s*(\d+)", result.tail)
            executions = int(units.group(1)) if units else 0
            passed = result.returncode == 0 and not result.timed_out and executions > 100
            results.append({"target": target, "seconds": seconds, "executions": executions,
                            "timed_out": result.timed_out, "passed": passed})
            if not passed and result.returncode == 0:
                raise RuntimeError("fuzzer did not execute enough inputs")
            if result.returncode:
                raise SystemExit(result.returncode)
    finally:
        (REPORT / "summary.json").write_text(json.dumps(results, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
