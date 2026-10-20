# vst-audio

Work toward a GUI-independent Rust library for hosting VST2 and VST3 plugins
in separate processes on Windows, macOS and Linux.

**0.1.0 is in development.** The initial extraction is a Windows implementation;
the cross-platform backends and public API are being developed and tested.
This is not yet a crates.io release.

The intended contract is one worker per plugin instance, safe application-side
Rust APIs, bounded audio IPC and worker-owned native editor windows. Any Rust GUI
can request an editor over the same control protocol. Native editor embedding
inside an application's window is a separate, platform-specific capability.
Process isolation contains crashes and hangs; it is not a security sandbox for
hostile code.

See [architecture](docs/architecture.md), [implementation plan](docs/roadmap.md)
and [source provenance](NOTICE).

VST is a registered trademark of Steinberg Media Technologies GmbH.
This project is independent of Steinberg and is not endorsed by it.
