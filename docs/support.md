# Implemented and pending support

This is a 0.1.0 development tree, not a completed general-purpose release.

| Area | Current implementation |
|---|---|
| Platforms | Windows x64, Linux x64/X11, macOS ARM64 and Intel native worker tests |
| Formats | VST2 and VST3 effect loading, scan, planar f32 processing, state |
| Isolation | One worker per instance, independent supervision, bounded control queues |
| Realtime API | Fixed caller-selected quantum, no allocation/locking/waiting in callback, explicit fallback |
| Parameter control | Paged metadata, normalized edits, explicit invalid-request errors |
| Editors | Floating worker-owned native windows; open/close through GUI-independent IPC |
| GUI consumers | egui and iced examples in a separate workspace |
| Linux event loop | VST3 IRunLoop from both host context and editor frame; timers and descriptors |
| Recovery | Low-level state history and retry policy; high-level failure/recovery notification |

The following approved roadmap work remains before calling 0.1.0 complete:

- Instruments and sample-offset MIDI/event input and output.
- Multiple audio buses, sidechain configuration and general channel layouts.
- f64, variable callback reblocking, transport context and sample-accurate automation.
- A complete editor show/hide/focus/scale contract and native-window visual evidence.
- High-level recovery that swaps prebuilt endpoints at audio block boundaries.
- Independent third-party public fixtures, extended stress/fuzz/sanitizer coverage.
- Final worker packaging, macOS distribution-signing documentation, MSRV and
  registry publication readiness.

Native embedding into the application's own window, native Wayland editor
hosting, cross-architecture bridging and adversarial-code sandboxing remain
distinct capabilities. None is silently substituted by in-process hosting.

CI functional tests do not certify hardware audio latency, broad commercial
plugin compatibility, accessibility or visual fidelity.
