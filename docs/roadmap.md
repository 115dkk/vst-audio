# 0.1.0 implementation and evidence plan

No version tag or package publication until the applicable release gates pass.

| Phase | Deliverable | Gate |
|---|---|---|
| P0 | Independent source, provenance, architecture and CI | Standalone build, no application dependencies |
| P1 | Process/IPC/editor probes on three OSes | Native runner execution, teardown and diagnostics |
| P2 | VST2 and VST3 scan/load/process/state | Numeric output verified for both on three OSes |
| P3 | Bounded realtime transport and supervision | Kill/hang A while B and parent keep processing |
| P4 | Instruments, events, buses, automation, transport | Format semantics and capability tests |
| P5 | Worker editor lifecycle and two unrelated Rust GUI examples | Real native window state and IPC agree |
| P6 | Recovery, stress and independent plugin compatibility | Public reproducible fixtures, no secrets needed |
| P7 | Packaged worker and crate consumption | Fresh consumer builds and runs packaged artifacts |

## CI

Required native matrix: Windows x64 (`windows-2025`), Linux x64
(`ubuntu-24.04`, Xvfb), macOS ARM64 (`macos-15`), macOS Intel
(`macos-15-intel`). Record runner image, toolchain, target and source revision.
Use WSL/WSLg for local Linux iteration; macOS GitHub runners are the primary
macOS development environment from P1.

Test layers: codec/state/ring unit and property tests; native test plugins;
crash/hang injection at every lifecycle phase; actual floating editor lifecycle;
two independent GUI consumers; package consumer outside the checkout;
sanitizers/fuzz/stress as extended jobs.

Fixtures must be built for the runner architecture. A missing fixture or display
in a required job is a failure, not a passing skipped test. Tests do not need a
physical audio device: deterministic blocks traverse the actual worker and IPC.
Realtime scheduling measurements are reported separately from functional
correctness; shared hosted runners cannot certify hardware latency.

Keep bounded logs, test reports, native crash diagnostics and available editor
captures on failure. No proprietary plugin, decrypted asset or secret is an
artifact. Only trusted release jobs receive signing credentials.

## Initial state

P0: source extracted from spatial-audio-softdriver b6d4df9.
P1–P7: pending. See commits and CI for subsequent evidence.
