# vst-audio

Work toward a GUI-independent Rust library for hosting VST2 and VST3 plugins
in separate processes on Windows, macOS and Linux.

**0.1.0 is in development.** Native VST2/VST3 fixture tests pass on Windows,
Linux, macOS ARM64 and macOS Intel. Broader format capabilities and compatibility
work are still in progress.
This is not yet a crates.io release.

The intended contract is one worker per plugin instance, safe application-side
Rust APIs, bounded audio IPC and worker-owned native editor windows. Any Rust GUI
can request an editor over the same control protocol. Native editor embedding
inside an application's window is a separate, platform-specific capability.
Process isolation contains crashes and hangs; it is not a security sandbox for
hostile code.

See [architecture](docs/architecture.md), [implementation plan](docs/roadmap.md)
and [source provenance](NOTICE).

The application API has a nonblocking loading task, bounded control requests,
polled events and an independent supervisor. Any GUI can use this contract:

```rust,ignore
let host = vst_audio::Host::new(worker_path);
let task = host.spawn_async(plugin, vst_audio::Configuration::default(), None);
// In the GUI update loop:
if let Some(instance) = task.try_complete()? {
    instance.controller().request(vst_audio::Command::OpenEditor)?;
    // Keep `instance` alive; dropping it requests worker shutdown.
}
```

`Instance::take_audio` transfers the unique preallocated audio endpoint.
It accepts planar f32 blocks of the configured quantum, returns processed or
fallback output, and reports pipeline + plugin latency. The supervisor runs
even when the GUI does not poll. Drop audio resources on a control thread.

Build and test:

```text
cargo test --workspace
python tools/check_boundary.py
python tools/native_tests.py
```

On headless Linux, run the last command under `xvfb-run -a`.
The CLI `cargo run -p vst-audio --example editor -- <worker> <plugin>` opens an
editor without a GUI framework.

VST is a registered trademark of Steinberg Media Technologies GmbH.
This project is independent of Steinberg and is not endorsed by it.
