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

### Linux floating editor backends

`Configuration::linux_editor` and `ProcessingOptions::linux_editor` preserve a
`LinuxEditor` choice across recovery. `EditorStatus.platform` reports the actual
backend. Other operating systems ignore the Linux choice. Embedded X11 windows
always use the session `DISPLAY`, regardless of that choice.

The intended three routes are native VST 3 Wayland, the session X display
(Xorg or desktop XWayland), and a private rootful Xwayland per editor. Native
Wayland and session X are implemented; **private Xwayland is not implemented in
this change**. `Auto` currently tries a supporting VST 3 view on Wayland and then
the session X display. `Wayland` rejects VST 2/unsupported views; `X11` never
exposes a Wayland host. Private Xwayland is planned, not implemented, and has no
public selection or wire tag. Both public backend enums are non-exhaustive.
`Auto` also retries a fresh view on X11 after Wayland setup or attachment fails;
explicit `Wayland` never falls back.

VST 2 cannot render natively to Wayland: Linux `effEditOpen` receives an X11
window ID, and existing plugin binaries call X11 themselves. Substituting a
Wayland pointer cannot change that ABI. A private X server must also arrange
for the plugin's own X connection to target that server, not merely change the
parent window ID. Changing process-global `DISPLAY` after worker threads have
started is not a safe solution. A pre-load display-routing/launch boundary is
needed before that route can be considered implemented.

For native Wayland the worker **is** the host UI. It opens one connection to
the session compositor and supplies its `wl_display*` through
`IHostApplication::createInstance(IWaylandHost)` or direct `queryInterface`. The worker's parent surface,
view subsurface and popup parents therefore belong to one real connection.
There is no nested compositor or request-delegating server. This differs from
an SDK EditorHost whose UI and plugin have separate connections. Direct
`queryInterface(IWaylandHost)` returns the same host identity as `createInstance`,
with an owned reference; X11-only workers expose neither route.
The connection normally remains retained through module/controller teardown;
balanced open/close calls are counted. Failed setup and lost connections release
it after view teardown, so the next open reconnects.

Linux-only `wayland-client` uses the libwayland system backend with runtime
`dlopen`. The backend creates an internal event queue and assigns its own
proxies to it; plugin proxies use the default queue or a guest/private queue.
On each main-thread iteration, IRunLoop descriptor handlers run first, then
the host dispatches its inner queue and flushes. Without a registered plugin
handler for the display fd, the host polls nonblocking, prepares/reads only
when readable, and dispatches pending default-queue events too. The host never
reads behind a registered handler that may already have drained the socket.

The xdg-shell parent uses shm background buffers, ping/pong, optional server
decoration and optional activation requests. Parent-buffer scale follows
preferred-buffer-scale or the largest entered-output scale; logical view size
is kept separate from physical buffer size. Optional view content scale is
passed before attachment and on subsequent scale changes. The VST 3.8 Wayland
header defines no additional DPI coordinate convention. Capture fails explicitly:
Wayland clients cannot read desktop pixels. Wayland cross-process embedding is
not supported. Without the decoration protocol (notably GNOME), windows have no
host-drawn title bar. Initial mapping waits up to five seconds for a fresh
configure and acknowledges it before attaching a buffer. Wayland Hide calls
`removed()`, releases the plugin view/frame, and destroys the entire parent
`wl_surface`, xdg roles, decoration and shm buffers. The editor stays open but
hidden, preserving its last size/accepted scale and the host display connection.
Show creates a fresh view and top-level through the initial mapping procedure,
restores size and scale, and attaches it. Failed recreation closes the editor
and emits `EditorClosed`; repeated Hide/Show in the corresponding state is a no-op.
Hidden Resize only records the next size (constraints are checked on recreation);
Focus is a no-op without a surface, Capture remains unsupported, and Scale fails
while no view exists. Parameters and plugin state remain in the controller.

This avoids Weston 13's missing configure when remapping a null-buffer-unmapped
xdg top-level. Reusing an old configure would violate xdg-shell. Recreating only
the xdg roles on the same surface is also unsuitable: a live plugin view may
retain the old pointers from `IWaylandFrame::getParentSurface`/`getParentToplevel`.
Destroying the view first avoids leaving those pointers stale. X11 continues to
unmap/map the same window and view. Background buffers are reused only after
release, with at most two live buffers per visible Wayland editor.
Initial surface synchronization precedes view attachment and content-scale setup;
rejected content scales leave the last accepted scale unchanged.

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

## VST 2 ABI and concurrency

Definitions originate in the pinned BSD-3-Clause Xaymar clean-room header named
in NOTICE, not Steinberg's VST 2 SDK. Three intentional ABI differences follow
the pinned ISC DPF wrapper: plugin-to-host events use opcode **8**, event pointers
are inline immediately after the event-list header, and callback integers use
pointer-width `isize`. DPF MIDI Through and native i686 interoperability are the
regression evidence for retaining those choices.

The main-thread wrapper and the unique audio processor share only immutable
copied entry pointers/metadata and atomic callback state. They never hold a Rust
reference to the foreign effect across a foreign call. Foreign plugins must
synchronize their own UI/DSP state, as required by the VST 2 concurrent-call
contract. Test fixtures likewise split UI-only and audio-only storage and use
atomic parameters. Admission is non-waiting on audio; only control waits for the
in-flight processing call.

| Calls | Close audio admission? |
|---|---|
| OPEN, CLOSE, SET_SAMPLE_RATE, SET_BLOCK_SIZE, MAINS_CHANGED | Yes |
| PROCESS_BEGIN/END, SET_SPEAKER_ARRANGEMENT, precision (77) | Yes |
| SET_CHUNK, SET_PROGRAM, post-lifecycle/header revalidation | Yes |
| Native editor pump, EDIT_OPEN/CLOSE/IDLE/GET_RECT | No |
| Parameter name/value/label queries and setParameter | No |
| GET_CHUNK, GET_PROGRAM, capability/category queries | No |

`ioChanged` atomically suspends new processing admission until the next control
pass re-reads input/output/parameter counts and latency. A latency-only change
emits `LatencyChanged` and resumes processing; a count change permanently rejects
processing and emits a restart-required Fault. The same check follows state,
configuration and resume. OPEN's final counts/flags become the execution snapshot.
The callback acknowledges that latency changes can be handled, **not** arbitrary
IO changes. `acceptIOChanges` is not advertised. A callback during an in-flight
foreign call cannot revoke that call; plugins must not start accessing an enlarged
buffer in the middle of that call.

Time records are initialized to stopped 48 kHz, 120 BPM, 4/4 before OPEN and reset
to the configured rate before rate/block callbacks. OPEN precedes Configure on
the wire, so it cannot know a future non-default sample rate. A sequence counter
around atomic fields gives coherent cross-thread snapshots. Its only writer is
the gated control thread before processing, then the audio thread; audio callback
reads cannot overlap another writer. Constant-initialized TLS identifies audio
(realtime 2/offline 4) versus other threads (1).

Host canDo returns 1 only for `sendVstEvents`, `sendVstMidiEvent`,
`receiveVstEvents`, `receiveVstMidiEvent`, `sendVstTimeInfo`, `sizeWindow`,
`startStopProcess`, `shellCategory`. All others, including `acceptIOChanges`,
`offline`, file selectors, `editFile`, `reportConnectionChanges` and
`sendVstMidiEventFlagIsRealtime`, return 0. Input strings are bounded to 128 bytes.
Events mean MIDI and bounded SysEx, not every historical VST event type.
`updateDisplay` and endEdit coalesce into `ParamsChanged`; gesture boundaries do
not cross the protocol yet.

Shell enumeration is bounded to 1024 entries. A child class ID is `b"VST2"` +
its signed unique ID's four big-endian bytes + eight zero bytes. Non-shell class
IDs remain all zero. Entry-time currentEffectID uses TLS even without an effect
pointer; the returned child's unique ID must match. The effect canDo helper
preserves yes/unknown/no, independently of category-based shell detection.

A layout identical to the loaded declaration is retained without renegotiation
(the app echoes this by default, and DPF Latency does not implement opcode 42).
Changed explicit mono (mask 4, speaker 0), stereo (mask 3, speakers 1/2), and 5.1
(mask 63, speakers 1..6) requests use effect opcode 42; an absent direction uses
an empty arrangement. Rejection fails setup. Other explicit masks fail with an
unavailable-layout error. Both masks zero retain the declared default without
requesting a new layout. Processing precision requires both its flag and pointer.

VST 2 chunks are opaque, including `SSV3` prefixes. The caller already selects
PluginFormat and class on Load, and recovery retains that descriptor; payload
sniffing cannot reliably establish format. Callers must associate exported state
with its original plugin/format. Non-chunk state is always `[program i32 LE,
parameters f32 LE...]`; zero-program plugins store zero and ignore that field on
restore. All normalized values are validated before modifying the plugin.

`sizeWindow` atomically publishes one packed width/height pair, only while an
editor is opening/open. UI-thread resize failures are diagnostic; status and
`EditorResized` (kind 31, request 0) use actual native dimensions. VST3 dimensions
are sampled on the control loop so `IPlugFrame::resizeView` also reaches the app,
without changing the frame implementation. Notifications coalesce, not a history
of intermediate sizes. A zero VST2 EDIT_OPEN reply is accepted only if a native
child HWND/X11 window/NSView exists. Post-open GET_RECT corrects initial size.
Host directory pointers last through CLOSE: UTF-8 on Unix and Windows ACP narrow
strings (unrepresentable Windows paths fail rather than return a wrong path).
The clean-room header specifies only `char*` on Windows, not an encoding;
ACP is the explicit legacy Windows policy, not a claim of universal UTF-8 support.
