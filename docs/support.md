# Support and verification

Version 0.1.0 is published on crates.io. Distributor credentials are separate
from this repository. Required CI executes the implementation on Windows x64, Linux x64/X11, macOS ARM64 and Intel.

| Area | Implementation and required evidence |
|---|---|
| VST2 / VST3 | Disposable scan, effects and instruments, state and native bundles; both formats execute numeric audio tests |
| Architectures | Bounded PE, ELF and Mach-O (including universal) header inspection; `Host::with_worker` registers architecture-specific workers and native slices are preferred. x86_64 Linux application + i686 worker verified with VST2/VST3 numeric audio, events, state and editors; Windows emulation and macOS Rosetta combinations require native CI verification and are not certified by cross-compilation |
| Isolation | One worker per instance; crash/hang A while parent and B keep processing |
| Audio | Planar f32/f64, variable callback sizes, preallocated reblocking and explicit fallback; allocator guard covers the callback |
| Events | Sample offsets and bus indices, MIDI channel messages and owned SysEx (up to 256 bytes); bounded 256-event packets and explicit host overflow |
| Automation / transport | VST3 parameter queues retain offsets; VST2 splits at automation boundaries; in-block transport changes split both formats |
| Buses | Multiple buses, sidechains, auxiliary outputs, active flags and general speaker masks; declared bus kind, name and default-active flag; activation chosen after loading (`BusSelector`, `BusSelector::main_only`); native order retained and negotiated layouts read back |
| Parameters | Paged IDs, values, steps, units and read-only/automatable/hidden attributes; invalid and read-only edits return errors |
| Editors | Floating worker-owned windows; open, hide, show, focus, close, status and bounded native-view capture |
| Editor negotiation | VST3 constrained resize and optional content-scale interface; VST2 returns explicit unsupported results for host-driven resize/scale |
| GUI consumers | Separate egui and iced example workspace; the same IPC API runs each editor lifecycle on all four native targets |
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

Native embedding into an application's own window, native Wayland editor
hosting, Linux aarch64-to-x86_64 emulation, 32-bit macOS and adversarial-code
sandboxing remain outside this 0.1.0 scope. Recovery starts a new instance and
does not replay old MIDI notes; applications can send current musical state
after `Recovered`.
A changed layout requires a newly prepared application audio endpoint.

CI pixel captures verify real native view content and lifecycle. They do not
replace manual accessibility, keyboard/IME or broad third-party UI testing.
Hosted runners do not certify device latency, dropout rates on physical audio
hardware or universal plugin compatibility. macOS artifacts are ad-hoc signed;
production signing/notarization is described in [distribution](distribution.md).
