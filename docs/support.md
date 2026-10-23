# Support and verification

Version 0.1.0 is published on crates.io. Distributor credentials are separate
from this repository. Required CI executes the implementation on Windows x64, Linux x64/X11, macOS ARM64 and Intel.

| Area | Implementation and required evidence |
|---|---|
| VST2 / VST3 | Disposable scan, effects and instruments, state and native bundles; both formats execute numeric audio tests |
| Architectures | Bounded PE, ELF and Mach-O (including universal) header inspection; `Host::with_worker` registers architecture-specific workers and native slices are preferred. x86_64 Linux application + i686 worker verified with VST2/VST3 numeric audio, events, state and editors; Windows emulation and macOS Rosetta combinations require native CI verification and are not certified by cross-compilation |
| Isolation | One worker per instance; crash/hang A while parent and B keep processing |
| Sandbox | Opt-in, off by default, fail-closed setup: Linux Landlock + seccomp, Windows AppContainer (network capabilities only when allowed) + job, macOS Seatbelt. Linux VST2/VST3 native probes cover TCP/write/exec, audio, editor and scan; Windows/macOS execution remains pending native CI, not certified by cross-compilation |
| Audio | Planar f32/f64, variable callback sizes, preallocated reblocking and explicit fallback; allocator guard covers the callback |
| Events | Sample offsets and bus indices, MIDI channel messages and owned SysEx (up to 256 bytes); bounded 256-event packets and explicit host overflow |
| Automation / transport | VST3 parameter queues retain offsets; VST2 splits at automation boundaries; in-block transport changes split both formats |
| Buses | Multiple buses, sidechains, auxiliary outputs, active flags and general speaker masks; declared bus kind, name and default-active flag; activation chosen after loading (`BusSelector`, `BusSelector::main_only`); native order retained and negotiated layouts read back |
| Parameters | Paged IDs, values, steps, units and read-only/automatable/hidden attributes; invalid and read-only edits return errors |
| Editors | Floating worker-owned windows; open, hide, show, focus, close, status and bounded native-view capture |
| Editor embedding | Worker-owned child of an application HWND or X11 window; parent destruction closes only the editor. Linux x86_64/X11 VST2/VST3 parent pixels, lifecycle and default sandbox and Windows x64 VST2/VST3 parent pixels and lifecycle are native-tested. Windows refuses a sandboxed (AppContainer) worker's child under the application window: that request fails explicitly, so sandboxed Windows editors float. Mixed DPI is untested. macOS returns explicit unsupported; no floating fallback |
| Editor negotiation | VST3 constrained resize and optional content-scale interface; VST2 post-open sizing and plugin sizeWindow. Both formats emit coalesced EditorResized with actual dimensions, floating and embedded. VST2 host-driven resize/scale remains unsupported |
| GUI consumers | Separate egui and iced example workspace; the same IPC API runs each editor lifecycle on all four native targets |
| Linux Wayland editor | Native VST 3 floating view, shared host display, xdg-shell, shm background, optional decoration/activation, logical resize and integer output scale. Tested on Weston 13 headless/pixman at scales 1 and 2, with and without default sandbox. Hide tears down the view; Show recreates it. Capture and embedding unsupported; other compositors unverified |
| Private Xwayland editor | Planned, not implemented; no public selection value. Auto stops after native Wayland/session X |
| Linux event loop | VST3 IRunLoop on host context and editor frame, with timers and file descriptors |
| Recovery | Saved-state history, bounded manual/automatic restart, generation changes and endpoint handover at audio boundaries; old resources reclaimed off audio |
| Stress | Variable callbacks and four repeated crashes per format, with 400 numeric healthy-peer callbacks and latest-checkpoint preservation |
| Independent plugins | DPF MIDI Through/Latency, Steinberg ADelay and mda DX10; MIDI values/offsets, actual DSP, rejected/accepted layouts and repeatability |
| Extended checks | Coverage-guided control/ring fuzzing plus AddressSanitizer and leak checks on native workers and public fixtures |
| Delivery | Seven crate archives consumed outside the checkout; optimized worker ZIPs extracted and executed; manifests, checksums and dependency licenses |
| Toolchain | Rust 1.99.0 minimum, tested directly; nightly is used only by the extended test lane |

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

## Linux editor verification and remaining work

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

The planned private-rootful-Xwayland route needs Xwayland 23.1 or later and one
extra process per editor. Its root confines menus/popups to the editor bounds;
clipboard integration is unverified. Without `-hidpi`, the compositor scales
its scale-1 buffer, potentially making it blurry. Process creation must be
allowed by the sandbox. Process startup, RandR resizing, hide/show, child reaping
and parent-death cleanup for this route are **not implemented or tested here**.

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

Run `xvfb-run -a python3 tools/native_tests.py` for the sandbox probes. The tests
first prove the unsandboxed fixture can connect/write/execute, then assert the
default, network-allowed and directory-allowed outcomes while numeric audio and
native editor lifecycle continue. Missing fixtures or OS support fail the test.

CI pixel captures verify real native view content and lifecycle. They do not
replace manual accessibility, keyboard/IME or broad third-party UI testing.
Hosted runners do not certify device latency, dropout rates on physical audio
hardware or universal plugin compatibility. macOS artifacts are ad-hoc signed;
production signing/notarization is described in [distribution](distribution.md).

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
