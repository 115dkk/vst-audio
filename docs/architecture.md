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
`OpenEditor` creates a floating window in the worker process. `OpenEditorIn`
accepts a framework-independent `ParentWindow` integer handle and creates a
worker-owned child inside the application's HWND (Windows) or X11 window (on
the worker's display). The plugin and its view remain entirely in the worker;
no plugin code is loaded into the application. There is one editor per worker:
changing parent or switching between floating and embedded closes the old view
before attaching the new one. Closing either editor leaves audio processing
alive. Worker death invalidates its editors.

`EditorStatus.embedded` identifies this mode. Size negotiation, visibility,
focus requests and bounded capture use the same contract in both modes. The
application sizes its placeholder from the reported width and height. Destroying
the parent destroys the child and emits `EditorClosed`. Hiding the parent
(minimizing, switching views) leaves the editor open; it reappears with the
parent. Handles must remain valid while opening, and applications must continue
pumping their native message loop.

**Windows cross-process parent/child windows attach their input queues. A hung
worker can therefore stall application input. Independent supervision must
terminate the hung worker to release that coupling.** This is crash containment,
not input isolation. Mixed-DPI hosting is a property of the parent at creation;
changing `SetThreadDpiHostingBehavior` in the worker cannot retrofit the application's
parent. The worker leaves DPI context/hosting behavior unchanged. Applications
must create a compatible parent; mixed-DPI combinations require native testing.
A sandboxed worker may be denied access to a higher-integrity parent by Windows;
that creation failure is an explicit request failure, with the native error
retained in worker diagnostics, never a floating-window fallback.

A raw NSView or wl_surface pointer cannot cross process boundaries. macOS
embedding explicitly returns unsupported because an NSView belongs to its
own process; native Wayland embedding is outside this contract. Floating editors
and a caller-rendered parameter UI remain the portable alternatives.

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
`Configuration::sandbox` and `ProcessingOptions::sandbox` optionally select an
OS sandbox; `None` preserves plugin compatibility. `SandboxPolicy` denies
network access, execution of other programs and writes outside per-worker
temporary storage / explicit writable directories by default. Sandboxed scans
use `Host::scan_sandboxed`, since module loading itself executes foreign code.
Unix workers install restrictions after opening IPC and before spawning helper
threads or loading plugins; Windows installs the token and job restrictions at
process creation. A pre-load confirmation supplies `Started::sandbox`, and
setup failures return `StartError` without loading the plugin. Recovery retains
the policy and creates fresh temporary storage. The parent owns that storage
until the child is terminated and reaped. Resource limits and full desktop
session isolation remain outside this policy; see the platform exceptions in
`support.md`.

## Platforms and delivery

Windows uses owned handles, jobs, mappings and native windows. Linux uses Unix
IPC, mappings, process supervision and X11 initially, with Wayland negotiated
separately. macOS uses native IPC, bundles, AppKit on the main thread and a
packaged worker app. The same IPC state machine is used on all three.

The consumer packages a matching worker executable beside their application.
Plugin headers select a registered architecture-specific worker when no native
slice exists; fixed-width IPC permits cross-architecture execution where the OS
supports it, and recovery keeps the selected worker path.
Startup negotiates protocol/version, without requiring matching architectures.
Cargo dependencies alone do not install executables. Development ad-hoc signing
and production Developer ID signing/notarization are separate paths.
