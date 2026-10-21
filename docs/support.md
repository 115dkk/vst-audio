# Support and verification

The approved 0.1.0 implementation is present. Registry publication, a version
tag and distributor credentials are separate release operations. Required CI
executes the implementation on Windows x64, Linux x64/X11, macOS ARM64 and Intel.

| Area | Implementation and required evidence |
|---|---|
| VST2 / VST3 | Disposable scan, effects and instruments, state and native bundles; both formats execute numeric audio tests |
| Isolation | One worker per instance; crash/hang A while parent and B keep processing |
| Audio | Planar f32/f64, variable callback sizes, preallocated reblocking and explicit fallback; allocator guard covers the callback |
| Events | Sample offsets and bus indices, MIDI channel messages and owned SysEx (up to 256 bytes); bounded 256-event packets and explicit host overflow |
| Automation / transport | VST3 parameter queues retain offsets; VST2 splits at automation boundaries; in-block transport changes split both formats |
| Buses | Multiple buses, sidechains, auxiliary outputs, active flags and general speaker masks; native order retained and negotiated layouts read back |
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

Native embedding into an application's own window, native Wayland editor
hosting, cross-architecture bridging and adversarial-code sandboxing remain
outside this 0.1.0 scope. Recovery starts a new instance and does not replay old
MIDI notes; applications can send current musical state after `Recovered`.
A changed layout requires a newly prepared application audio endpoint.

CI pixel captures verify real native view content and lifecycle. They do not
replace manual accessibility, keyboard/IME or broad third-party UI testing.
Hosted runners do not certify device latency, dropout rates on physical audio
hardware or universal plugin compatibility. macOS artifacts are ad-hoc signed;
production signing/notarization is described in [distribution](distribution.md).
