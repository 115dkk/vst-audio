# Working agreement

Read `docs/architecture.md`, `docs/roadmap.md` and `NOTICE` before changing the
design. Work on a branch and land it on `main` through a pull request. `main`
receives only squash merges and the version bump commits of the release
workflow.

- Keep the application API independent of GUI frameworks and audio devices.
- Never load foreign plugins in the application process. Scan in disposable workers.
- Default to one plugin instance per worker.
- Unsafe belongs in `-sys` crates and test-only plugin fixtures. Other crates
  forbid unsafe. Explain each unsafe operation's actual preconditions.
- The realtime application path never allocates, locks or waits for a worker.
- Linux, Windows and macOS native execution are required; cross-compilation
  alone is not platform verification. macOS is developed using GitHub runners.
- Preserve source/license provenance. Do not read or copy SpatialAudioLab code.
- Public documentation distinguishes implemented, tested and planned behavior.
- Commit messages and pull request titles use a conventional prefix and Korean
  description. The squash merge turns the title into the `main` commit that
  `tools/release_gate.py` and the release notes read: `feat`, a `!` after the
  type or a `BREAKING CHANGE:` footer makes the next release a minor bump, and
  anything else a patch. Cargo treats 0.x patch releases as compatible, so a
  pull request that breaks a public interface must carry the `!`.
- Before committing run fmt, clippy and relevant tests; native platform checks
  that are unavailable locally must run in CI.
