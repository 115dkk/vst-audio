# Support and verification

Version 0.1.0 is published on crates.io. Distributor credentials are separate
from this repository. Required CI executes the implementation on Windows x64, Linux x64/X11, macOS ARM64 and Intel.

| Area | Implementation and required evidence |
|---|---|
| VST2 / VST3 | Disposable scan, effects and instruments, state and native bundles; both formats execute numeric audio tests |
| Architectures | Bounded PE, ELF and Mach-O (including universal) header inspection; `Host::with_worker` registers architecture-specific workers and native slices are preferred. CI runs the native, feature and cross-architecture tests (VST2/VST3 numeric audio, events, state and editors) with a foreign worker on four native runners: Linux x86_64 + i686, Windows x64 + i686, Windows ARM64 + x64 (emulation) and macOS arm64 + x86_64 (Rosetta) |
| Isolation | One worker per instance; crash/hang A while parent and B keep processing. Windows creates workers without a console window (`CREATE_NO_WINDOW`), so a GUI application does not open one per instance |
| Sandbox | Opt-in, off by default, fail-closed setup: Linux Landlock + seccomp, Windows AppContainer (network capabilities only when allowed) + job, macOS Seatbelt. VST2/VST3 native probes for TCP, writes outside the allowed folders and process creation, plus audio, editor and scan, run on Linux, Windows and macOS CI runners for every policy. On Windows a network-allowed worker reaches loopback only after an administrator exempts the `vst-audio.worker.network` profile (`CheckNetIsolation LoopbackExempt`) |
| Audio | Planar f32/f64, callbacks up to configured `max_callback_frames`, `Configuration::slots` (2, 4, 8 or 16) per lane, modulo-2^32 ring sequences, `BlockEndpoint`/ring-only `BlockLane`, fixed-depth preallocated reblocking and explicit fallback; allocator guards cover block and callback paths, and native tests cover 480-frame callbacks over a 128-frame quantum |
| Events | Sample offsets and bus indices, MIDI channel messages and owned SysEx (up to 256 bytes); bounded 256-event packets and explicit host overflow |
| Automation / transport | VST3 parameter queues retain offsets; VST2 splits at automation boundaries; in-block transport changes split both formats |
| Buses | Multiple buses, sidechains, auxiliary outputs, active flags and general speaker masks; declared bus kind, name and default-active flag; activation chosen after loading (`BusSelector`, `BusSelector::main_only`); native order retained and negotiated layouts read back |
| Parameters | Paged IDs, values, steps, units and read-only/automatable/hidden attributes; invalid and read-only edits return errors; plugin-originated begin/perform/end edits are bounded, ordered notifications with explicit coarse refresh on loss; a VST3 editor burst larger than the processor queue still delivers each parameter's latest value to the processor |
| Editors | Floating worker-owned windows; open, hide, show, focus, close, status and bounded native-view capture. `EditorScaling` selects automatic, plugin-only, system-stretched or unscaled opening; status reports the resolved mode |
| Editor embedding | Worker-owned child of an application HWND or X11 window; parent destruction closes only the editor. Linux x86_64/X11 VST2/VST3 parent pixels, lifecycle and default sandbox and Windows x64 VST2/VST3 parent pixels and lifecycle are native-tested. Windows refuses a sandboxed (AppContainer) worker's child under the application window: that request fails explicitly, so sandboxed Windows editors float. Real mixed-DPI multi-monitor setups remain unverified. macOS returns explicit unsupported; no floating fallback |
| Editor negotiation | VST3 constrained resize and optional content-scale interface; VST2 post-open sizing, plugin sizeWindow and optional `supportsViewDpiScaling` vendor scaling. Both formats emit coalesced EditorResized with actual dimensions, floating and embedded. VST2 host-driven resize remains unsupported |
| GUI consumers | Separate egui, iced, Slint and Tauri example workspace; the same IPC API runs each editor lifecycle on all four native targets. The Tauri page drives the lifecycle through its webview and IPC bridge |
| Linux Wayland editor | Native VST 3 floating view, shared host display, xdg-shell, shm background, optional decoration/activation, logical resize and integer output scale. Tested on Weston 13 headless/pixman at scales 1 and 2, with and without default sandbox. Hide tears down the view; Show recreates it. Capture and embedding unsupported; other compositors unverified |
| Private Xwayland editor | Not implemented, by decision (see the Linux editor section). VST 2 and X11-only VST 3 editors use the session X display; `Stretch` in a Wayland session uses the desktop's XWayland |
| Linux event loop | VST3 IRunLoop on host context and editor frame, with timers and file descriptors |
| Recovery | Saved-state history, bounded manual/automatic restart, generation changes and endpoint handover at audio boundaries; old resources reclaimed off audio |
| Stress | Variable callbacks and four repeated crashes per format, with 400 numeric healthy-peer callbacks and latest-checkpoint preservation |
| Independent plugins | DPF MIDI Through/Latency, Steinberg ADelay and mda DX10; MIDI values/offsets, actual DSP, rejected/accepted layouts and repeatability. On Windows, 3 Band EQ and MVerb output is compared with EQ APO XT's engine in both directions |
| Extended checks | Coverage-guided control/ring fuzzing plus AddressSanitizer and leak checks on native workers and public fixtures |
| Delivery | Seven crate archives consumed outside the checkout; optimized worker ZIPs extracted and executed; manifests, checksums and dependency licenses |
| Toolchain | Rust 1.99.0 minimum, tested directly; nightly is used only by the extended test lane |

`RecoveryPolicy::Watch` exposes the low-level 0.5/2/8-second restart schedule to
high-level instances, preserves two state snapshots across workers, and disables
automatic recovery after three failures in ten minutes.

The [independent-plugin ledger](independent-plugins.md) records the exact pinned
upstreams and assertions. DPF's pinned VST3 adapter has no SysEx input conversion
and no program-change parameter; these are recorded as upstream limitations.
The host's SysEx ownership and transport are separately verified through both
native-format fixtures. `events_dropped` reports host-detected rejection and
overflow, not events a plugin internally ignores. VST3 automation fidelity
ultimately depends on the plugin honoring the supplied sample offsets.

Architecture routing permits x86 workers on x86_64 Windows/Linux, x86 and
x86_64 workers on Windows ARM64, and x86_64 workers on Apple Silicon. The OS
must supply the necessary multilib/emulation/Rosetta support; `runs_on_host`
describes platform compatibility, not an installation probe. Unreadable or
unsupported binary headers retain the native-worker fallback. Run
`xvfb-run -a python3 tools/cross_arch_tests.py i686-unknown-linux-gnu` on Linux;
on Windows/macOS run the script without Xvfb with the desired Rust target.

Native macOS cross-process editor embedding, Wayland cross-process embedding,
private Xwayland hosting, Linux aarch64-to-x86_64 emulation and 32-bit macOS remain outside this
0.1.0 scope. The optional sandbox does not bound CPU or memory exhaustion,
isolate same-session window messages/input, or protect against kernel vulnerabilities. Recovery starts a new instance and
does not replay old MIDI notes; applications can send current musical state
after `Recovered`.
A changed layout requires a newly prepared application audio endpoint.

## Editor scaling

`Command::OpenEditor` and `OpenEditorIn` use `EditorScaling::Auto`.
Use `Command::OpenEditorWith { parent, scaling }` or
`PluginControl::open_editor_with(parent, scaling)` for an explicit mode.
`EditorStatus.scaling` reports `Plugin`, `Stretch` or `Off`, not the requested
`Auto`. `scale` is the factor sent to the plugin, or 1.0 when none was sent.

| Mode | Windows floating | Windows embedded / X11 / Wayland session |
|---|---|---|
| `Auto` | Plugin scaling when accepted; otherwise system stretching | Plugin scaling when accepted; otherwise 1:1 |
| `Plugin` | Plugin scaling when accepted; otherwise 1:1 | Same |
| `Stretch` | System-stretched 1:1 drawing; may be blurry | Wayland session with the desktop's XWayland (`DISPLAY`): a floating X11 window at scale 1, which the compositor scales as its setting for X11 applications decides; embedded, plain X11 or no `DISPLAY`: request error |
| `Off` | No plugin scale request; 1:1 physical pixels | No plugin scale request |

macOS retains its existing Retina behavior for every mode. Its status is `Off`
unless the VST3 view has accepted a content-scale request.
VST2 support requires `effCanDo("supportsViewDpiScaling") == 1`. The host sends
`effVendorSpecific` with `PreS` / `AeCs` before the first editor rectangle query
and once more after `effEditOpen`, then rereads the rectangle; explicit
`EditorAction::Scale` uses the same request and rereads the rectangle.
JUCE answers the canDo from 5.3 on. Its VST2 wrapper keeps a scale received
before the editor exists in 5.3 to 5.4 and from 6.1.3 on, but 6.0.0 to 6.1.2
drop it and apply only a request made while the editor is open, which is why the
request is repeated. JUCE on macOS ignores the request, and JUCE plugins on Linux
do not follow `Xft.dpi` themselves, so without the request their editors stay
at scale 1. These facts come from the JUCE sources of each release; plugins
built with JUCE 6.0.7, 6.1.3 and 7.0.5 were also measured at 96 and 192 DPI
under Xvfb. iPlug2 does not advertise `supportsViewDpiScaling`.
A plugin-scaled VST2 editor is reset to scale 1 during teardown so a subsequent
unscaled open does not inherit the old factor. An explicit scale request on a
stretched Windows editor recreates the window in plugin-scaling mode.
VST3 requires `IPlugViewContentScaleSupport` and a successful
`setContentScaleFactor`, offered before attachment.

Windows editor DPI handling needs Windows 10 version 1809 or later; the worker
looks its functions up at run time, so on older systems it still starts and
editors open with the default awareness at scale 1 (mixed hosting needs 1803,
stretched windows 1809).
Editor creation scopes use per-monitor-v2 awareness, except stretched windows,
which use `DPI_AWARENESS_CONTEXT_UNAWARE_GDISCALED`. The previous thread
context is restored. For a differently aware parent, the child creation scope
also requests mixed hosting and restores it afterwards; this does not change
the application's existing parent hosting policy. Initial scale comes from the
editor window's DPI, or the parent's DPI when embedded. `WM_DPICHANGED` rescales
plugin-scaled views and uses their new size; otherwise it uses the suggested
rectangle. Native Windows CI must verify these contexts and synthetic DPI
changes. Real mixed-DPI multi-monitor transitions are not verified by CI.

## Linux editor verification and remaining work

X11 reads `Xft.dpi` from the display root's `RESOURCE_MANAGER` property when an
editor opens, including embedded editors. The factor is DPI / 96, without
rounding, clamped to 0.5–4.0; missing or invalid values use 1.0. Native Wayland
continues to use compositor integer output scale for `Auto` and `Plugin`;
`Off` skips plugin content-scale requests. X11 tests cover fixture sizes and
pixels at 96 and 192 DPI, including a fixture mode that drops a scale received
before the editor exists; the independent DPF Parameters and JUCE (IEM) VST2
checks compare their sizes at both DPIs through the public API.

On Linux the worker loads libX11 before the plugin, when the library is
installed, and registers an Xlib error handler that logs protocol errors.
Xlib's default handler exits the process, a plugin that is not a standalone
application leaves error handling to its host, and JUCE loads libX11 itself
only when its editor first opens, so the handler has to be in place first.

Run `python3 tools/wayland_tests.py`: Weston is required, not silently skipped.
The native Wayland fixture is tested without `DISPLAY`; scale-2 captures show a
320×200 logical view as 640×400 physical pixels. Hide removes all fixture-color
pixels. Hiding a Wayland editor tears down its view; showing it creates a new
view. Parameters and state remain in the controller. The display connection stays
open, and the last size and scale are retained; a resize while hidden is applied
on Show. Native tests check two hide/show cycles, view creation/attachment/removal
and destruction counts, hidden resize, and continued audio. Growing a window can
clip against the compositor output because placement belongs to the compositor. The same script starts
Weston's desktop XWayland and runs existing VST2/VST3 floating, capture and
embedded-editor tests, including default-sandbox embedding.

Native Wayland has no capture API and no application-window embedding. On
compositors without server-side decoration (including GNOME), no title bar is
drawn by this host. Native tests cover initial configure, pre-attachment scale 2,
scale rejection retaining scale 1, Auto fallback to X11 after Wayland attachment
rejection, and EditorClosed plus continued audio after compositor exit. Direct
host query and createInstance return the same interface. Other compositors,
keyboard/IME and popup behavior have not been tested.

A private rootful Xwayland per editor was planned and is not implemented.
Plugins open their own X connection from `DISPLAY`, so a private server would
have to be chosen before the plugin loads, or `DISPLAY` changed in a worker whose
plugin threads may read the environment. The server also cannot be stopped while
a plugin holds a connection: Xlib's default I/O error handler exits the process.
Xwayland 23.2 (Ubuntu 24.04) has no `-hidpi`; probes on it confirmed rootful
startup with `-geometry`, `-displayfd` and `-decorate`, and RandR custom modes.
VST 2 and X11-only VST 3 editors therefore need the session X display (desktop
XWayland on GNOME, KDE and most compositors); without one they fail with an
explicit error. `EditorScaling::Stretch` in a Wayland session opens the editor on
the desktop's XWayland at scale 1, and the compositor scales it as its setting for
X11 applications decides (GNOME and KDE can also leave X11 windows unscaled).
`tools/wayland_tests.py` checks the routing under Weston's XWayland, not the
compositor's scaling.

## Sandbox policy and platform exceptions

`SandboxPolicy::default()` denies network and new program execution, and has no
additional writable directories. `network: true` permits licence/content access;
`processes: true` permits new programs. Writable paths must already exist and be
directories; they are canonicalized before launch. A sandboxed worker receives a
fresh private temporary directory through `TMPDIR`, `TMP` and `TEMP`; a worker
without a sandbox inherits the application's environment unchanged. Unix
temporary directories are mode 0700. Parent-owned cleanup runs after worker
teardown, including failed startup. A parent crash can leave temporary storage.
`Started::sandbox` reports the installed mechanism and denied categories.
Recovery retains the launch policy. Missing kernel support, rejected profiles,
ACL/token errors and absent worker confirmation are startup errors, never an
unsandboxed fallback.

- **Linux:** requires Landlock ABI >= 3 so truncate and refer permissions are
  expressible. The report includes the runtime ABI. Read/execute access remains
  global; write access is restricted to the private temporary directory, the
  allowlist, `/dev/null` and `/dev/shm`. ABI >= 5 also restricts device ioctls.
  Preopened shared-memory descriptors keep working. Seccomp checks the audit
  architecture, rejects x32 on x86_64, denies Internet/packet socket creation
  when requested, and denies `execve`/`execveat` when requested. Unix sockets
  remain available for X11 on x86_64. Legacy i686 `socketcall` socket/socketpair
  creation is denied when network is denied (classic BPF cannot inspect its
  pointed-to arguments); other legacy operations and direct AF_UNIX socket
  syscalls remain available. The additional i686 sandbox probe reached audio but
  failed editor opening with EPERM: this libc/X11 combination uses legacy socket
  creation. **Sandboxed i686 editors with network denied are not verified/supported**;
  ordinary unsandboxed i686 cross-architecture tests pass. `io_uring_setup` is denied to
  avoid bypassing socket filtering. This is not IPC isolation: a cooperating
  Unix-socket service can proxy network/filesystem operations, and `/dev/shm`
  remains shared. Linux process policy prevents exec, not fork-only children.
- **Windows:** every sandboxed worker runs in a persistent AppContainer profile:
  `vst-audio.worker` with no capabilities, or `vst-audio.worker.network` with
  `internetClient`, `internetClientServer` and `privateNetworkClientServer` when
  network is allowed. Windows blocks AppContainer loopback, so a network-allowed
  worker reaches local licence services only after an administrator exempts that
  profile (`CheckNetIsolation LoopbackExempt -a -n=vst-audio.worker.network`); the
  separate profile keeps such an exemption away from network-denied workers.
  Read/execute ACEs are added to the worker/plugin and containing directories
  for sibling DLLs and resources; writable directories and private temporary
  storage receive modify ACEs and inheritable Low integrity labels. Existing ALL
  APPLICATION PACKAGES read/execute grants are reused. Added ACEs and labels are
  **not removed**. Thus each fixed container SID shares grants across instances
  and runs; previously allowed directories remain accessible. AppContainer's own
  storage and paths already granting package access are OS exceptions, not an
  exact per-instance filesystem allowlist. The worker verifies its own token:
  AppContainer, and network capabilities exactly when the policy allows network.
  A restricted Low-integrity token was tried for the network-allowed case and
  abandoned: on Windows Server 2025 CI it could not read files whose DACL granted
  the user. The one-active-process job blocks children when `processes` is false.
  Inherited IPC handles retain their granted access. GUI, inherited-handle
  behavior and ACL propagation are exercised by Windows CI.
- **macOS:** uses the deprecated but available `sandbox_init` SBPL API. Reads
  remain unrestricted, IP outbound/bind and exec/fork are denied as requested,
  and writes outside canonical allowed paths and `/dev/null` are denied. The
  AppKit runtime is initialized before restriction for WindowServer setup, but
  no foreign plugin is loaded before restriction. Native AppKit/WindowServer and
  profile acceptance still require macOS CI. SBPL escaping and profile generation
  are unit-tested on Linux.

These policies are defense in depth, not a guarantee that arbitrary hostile
native code cannot interact with the user's session. They deliberately retain
GUI IPC and read access and do not replace stronger per-app isolation.

The shared audio mapping is created by the application and the worker receives
a writable descriptor to it, with or without a sandbox. On Linux that file is a
`memfd` sealed against shrinking and growing, so a compromised worker cannot
remove pages under the application's view. Windows pagefile-backed sections
have a fixed size. macOS has no file seals: a compromised worker that truncates
the private temporary backing file faults the application (`SIGBUS`) the next
time it touches the mapping. This is a known macOS limitation of the current
transport, not something the sandbox policy prevents.

Run `xvfb-run -a python3 tools/native_tests.py` for the sandbox probes. The tests
first prove the unsandboxed fixture can connect/write/execute, then assert the
default, network-allowed and directory-allowed outcomes while numeric audio and
native editor lifecycle continue. Missing fixtures or OS support fail the test.

CI pixel captures verify real native view content and lifecycle. They do not
replace manual accessibility, keyboard/IME or broad third-party UI testing.
Hosted runners do not certify device latency, dropout rates on physical audio
hardware or universal plugin compatibility. macOS artifacts are ad-hoc signed;
production signing/notarization is described in [distribution](distribution.md).

## Synthetic editor input verification

`tools/third_party.py` builds pinned DPF Parameters, SendNote and DPF-Plugins
3 Band EQ outside the checkout. The ignored tests inject XTest input on Linux
and SendInput on Windows into embedded editors; they do not call plugin UI
functions. Windows requires an interactive desktop: foreground denial, UIPI or
a target belonging to another process fails explicitly. On the windows-2025
runner SendInput reached the plugin windows: the EQ drags and SendNote keys
passed in both formats. The Parameters pixel check first failed because each
worker opened a console window over the editor; workers are now created with
`CREATE_NO_WINDOW`. For deterministic software rendering on GPU-less runners,
as Mesa serves Xvfb on Linux, the Windows run places a pinned Mesa llvmpipe
(`opengl32.dll`, `libgallium_wgl.dll`, `GALLIUM_DRIVER=llvmpipe`) beside each
plugin binary in the external build tree. The worker loads plugins with
`LOAD_WITH_ALTERED_SEARCH_PATH`, so a plugin's imports resolve from its own
folder before System32 and never from the worker's folder.
macOS reports `known limitation: macOS synthetic editor input` because embedding
is unsupported and synthetic input needs accessibility authorization. Wayland
input is outside this test.

Linux/Xvfb (1280x800x24) execution verifies VST 2 and VST 3 Parameters
gestures, pixels, gap-click negative control, host-to-UI updates and
fresh-instance state, both with and without the default sandbox. SendNote in
both formats verifies A4 frequency/RMS and release silence. EQ in both formats
verifies low-band gain and a background-drag negative control. The VST 3 cases
use host-created, bounded `IMessage` and `IAttributeList` objects. Tests close
each editor before destroying its X11 application parent; the plugin teardown
therefore runs while the embedded window hierarchy is still valid.

The test-only `__test-drop-editor-edits` feature suppresses VST 3 processor queue
updates while retaining edit notifications. Never enable it in distribution
builds. The runner builds a separate mutant worker and requires EQ to fail with
`editor-to-processor sound assertion failed`; the UI/controller value still
rises while the measured sound delta remains zero. A failed ordinary suite does
not prevent the mutation diagnostic from running.

DPF is pinned to `b801214e9ce41b56907e1c6c84859b6b4536917d`. The runner uses
DPF's CMake wrapper and `dpf_add_plugin` for MIDI Through, Latency, Parameters,
SendNote and the DPF-Plugins 3 Band EQ sources. The windows-2025 runner passes
every case in both formats; macOS reports the known limitation above. Plugin
binaries and external build trees must not be cached or uploaded; only bounded
logs and JSON reports are suitable CI artifacts.

## EQ APO XT engine comparison (Windows)

On Windows `tools/third_party.py` also downloads EQ APO XT v2.60.6 (x64-sse2
full package, SHA-256 pinned) and extracts its application folder into the
external build tree. Nothing is installed: no driver, service or registry
entry. EQ APO XT is GPL software and serves only as an independent engine
under test; none of its code is read for or copied into these crates. The
comparison uses effect plugins only, since EQ APO XT is not a MIDI or
instrument host: DPF-Plugins 3 Band EQ and MVerb (reverb), each as VST 2 and
VST 3.

Each case runs two directions with the same 48 kHz signal: a logarithmic sweep
from 20 Hz to 20 kHz at -18 dBFS, uniform noise at -30 dBFS and a -6 dBFS
impulse every 2 s, lasting 20 s (EQ) or 60 s (MVerb). vst-audio renders it in
512-frame blocks and drops its pipeline delay.

- **Forward:** the EQ APO XT Editor opens the plugin panel through UI
  Automation and SendInput drags two of the plugin's controls; the editor's
  pixels must change. After OK and Save, vst-audio applies the saved line
  (VST 2 values by name; VST 3 `ChunkData` restored and saved back byte for
  byte) and the sound must change by more than -60 dBFS against the defaults.
- **Reverse:** vst-audio sets the values and writes the line EQ APO XT
  reads. For VST 2 it writes every visible, writable parameter, renders, and
  writes each value by name as the shortest decimal of the plugin's f32. For
  VST 3 it saves the component state after processing a few blocks, restores
  it into a fresh instance that renders, and writes the state as `ChunkData`.

Benchmark.exe writes 16-bit PCM, so the comparison has two parts. The plain
run must line up at lag 0 (searched over ±64 frames) and differ from
vst-audio by at most 2 LSB. The difference run computes inside the engine:
the input carries `[x_L, x_R, y_L, y_R]`, the plugin processes channels 1 and
2, `Copy` subtracts vst-audio's output and `Preamp: 60 dB` raises the
residual, which must stay at or below -120 dBFS with no growth between the
first and last 5 s. The residual's resolution is about -156 dBFS. A one-sample
delay and a 2e-5 gain error of vst-audio's output are negative controls and
must exceed -120 dBFS.

In CI every comparison meets these bounds: each plain run differs from
vst-audio by at most 0.501 LSB (16-bit rounding), each residual is zero at the
output's resolution, and the controls read -60 dBFS (the one-sample delay,
limited by the 16-bit output) and about -100 dBFS (the gain error). Both
directions run for both plugins in both formats.

EQ APO XT shows the following behavior. These are properties of that engine
or of the plugin, recorded by the test, and not vst-audio defects.

- **VST 3 panel edits before v2.60.4:** v2.60.3 kept the default state in
  `ChunkData` for both DPF plugins although the panel's pixels changed (3973
  and 408 pixels). While no audio ran, its Editor passed a panel edit to the
  processor through a zero-sample process call, which a DPF component returns
  from before it reads parameter changes, and then saved the component state.
  EqualizerAPO-XT#437 passes the edit with one silent frame instead; the test
  now requires the saved state to differ from the default for both formats.
- **VST 2 values by parameter number are ignored:** a token that begins with
  a digit is read as a value, so a line of `<index> <value>` pairs leaves the
  defaults, thousands of LSB away from vst-audio. The Editor writes names.
- **Benchmark output parameter changes:** Benchmark.exe passes no output
  parameter change list to VST 3 `process()`; DPF prints an assertion message
  and continues.
- **VST 2 values saved before v2.60.5:** the Editor wrote VST 2 parameter
  values with six significant digits, so a saved value could read back as a
  different float. EqualizerAPO-XT#439 writes the shortest decimal that reads
  back as the same float.
- **Configuration encoding before v2.60.6:** a `config.txt` that starts with a
  UTF-8 byte order mark lost its first command (EqualizerAPO-XT#440). The test
  writes UTF-8 without a BOM.
- **3 Band EQ initial crossover:** the pinned 3 Band EQ reports a 220 Hz low
  crossover but filters at 200 Hz until "Low-Mid Freq" is written. EQ APO XT
  applies every parameter when it loads a line or a chunk, and the reverse
  direction does the same on the vst-audio side. For VST 3 that takes a
  restore, because a DPF VST 3 processor ignores a write of the value it
  already holds.

For a DPF VST 3 plugin, a vst-audio `SaveState` reflects `SetParameter` only
after a processed block, because the processor receives parameter changes in
`process()`. An application that saves state with audio running is unaffected.

## VST 2 hardening scope

Shell scan/child selection, truthful canDo, coherent pre-processing time snapshots,
post-OPEN metadata, lifecycle latency refresh, parameter refresh notifications,
and restart-required structural IO changes are implemented. Explicit mono,
stereo and 5.1 arrangements are negotiated; other non-default masks are rejected,
not silently accepted. An unspecified layout retains the declaration. VST3's
general speaker-mask support should not be inferred for VST2.

Linux/X11 fixture tests cover shell child gains, latency changes, state round trips,
OPEN-time channel changes, editor sizing, floating/embedded resize notifications,
and continued DSP during a 300ms editor idle. Windows/macOS have compile checks;
these new behaviors still require their native CI runs. Incompatible-count tests
prove fault/rejected DSP; automatic replacement is provided by existing supervision,
not a newly verified layout-recovery scenario in these tests.

VST2 edit gestures on the application protocol, plugin-formatted display strings,
program listing/selection, tail length and native bypass remain outside this
scope. Non-chunk state contains only the current program's values, not a complete
bank; chunks are bounded to slightly under 64 MiB. OPEN gets the host default
48 kHz because configuration arrives later; resume gets the configured rate.
