# Architecture: application proxy and isolated workers

Status: accepted design for 0.1.0, 2026-10-20.

## Boundaries

The application owns the safe `Host`, controller and realtime audio endpoint.
It never loads plugin binaries, including while scanning. A disposable scanner
or one dedicated worker per instance loads foreign code. Native editor windows
and their event loop belong to the worker's main thread. Plugin processing runs
on its audio thread following the format's threading contract. Independent
application supervision detects process death, startup/control deadlines and
audio stalls, without waiting on the control writer.

The target dependency boundary is:

- `vst-audio`: safe application API and supervision.
- `vst-audio-protocol`: bounded codec, identities, protocol negotiation and state machines.
- `vst-audio-ring`: portable atomic audio ring algorithms.
- `vst-audio-transport-sys`: OS processes, IPC, mapping and wakeup ownership.
- `vst-audio-worker-sys`: foreign ABI, dynamic modules and native editor windows.
- `vst-audio-worker-runtime`: worker lifecycle, main/audio coordination.
- `vst-audio-worker`: packaged worker executable.

The application dependency graph is checked by `tools/check_boundary.py`.
It cannot depend on the worker runtime, foreign loader or GUI frameworks.

## GUI contract

No Tauri, egui, iced, GTK, Qt or Slint types appear in the public contract.
Requests are nonblocking and identified by session/generation/request IDs.
Events are polled or delivered to an application-owned event loop adapter.
Opening, showing, hiding, resizing, focusing and closing an editor are control
requests. Focus is a request to the OS, not a guarantee of focus stealing.
Opening a native editor creates a floating window in the worker process.
Closing it leaves audio processing alive. Worker death invalidates its editors.

A raw NSView or wl_surface pointer cannot cross process boundaries. HWND/X11
embedding needs separate platform-specific lifetime, input and DPI handling.
Floating editors and a caller-rendered parameter UI are the portable contract;
embedding is negotiated and must not silently load a plugin into the parent.

## Data planes

Control frames have explicit byte order, version, length, bounded payloads and
request IDs. Bulk state transfer must not starve lifecycle or watchdog traffic.
Queued work and notifications have bounded storage and explicit overflow.
The wire ABI never contains Rust object layout or process-local pointers.

Realtime blocks use preallocated shared memory, sequence numbers, configuration
generations and sample positions. The parent trusts its own validated geometry.
Plugin processing receives private scratch buffers, never the shared mapping.
Late outputs cannot be substituted for a different sample interval.
The callback does not allocate, block or perform synchronous control I/O.
Fixed pipeline latency and plugin latency are reported in frames.

Native pointer/lifetime operations are audited in sys crates. For loading a
plugin, crash isolation does not make unknown native code memory-safe inside the
worker. Safe application APIs must not expose in-process loaders.

## Formats

The public model describes buses, f32/f64 precision, parameters and gestures,
sample-offset events, transport context, opaque state, latency, tail and editor
capabilities. VST2 indexes and VST3 parameter IDs retain their identity.
Unsupported features return explicit results; capabilities do not pretend the
formats have identical semantics. Bus activation is chosen by the application
selector after the plugin declares its buses; without one, every bus is active.
Reconfiguration invalidates old audio handles.

## Failure and recovery

Crashes and hangs are confined to one worker. Effects can use latency-aligned
bypass, silence or caller-provided fallback; instruments use silence. Recovery
creates a new process and session, restores the last successful state and
reconciles transport/notes. Past MIDI/automation is not blindly replayed.
Hostile-process protection requires additional OS sandboxing and resource limits.

## Platforms and delivery

Windows uses owned handles, jobs, mappings and native windows. Linux uses Unix
IPC, mappings, process supervision and X11 initially, with Wayland negotiated
separately. macOS uses native IPC, bundles, AppKit on the main thread and a
packaged worker app. The same IPC state machine is used on all three.

The consumer packages a matching worker executable beside their application.
Startup negotiates protocol/version/architecture. Cargo dependencies alone do
not install executables. Development ad-hoc signing and production Developer ID
signing/notarization are separate paths.
