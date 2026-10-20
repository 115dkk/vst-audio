# Working agreement

Read `docs/architecture.md`, `docs/roadmap.md` and `NOTICE` before changing the
design. The 0.1.0 work is authorized directly on `main`, using incremental
commits; do not create worktrees or pull requests.

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
- Commit messages use a conventional prefix and Korean description.
- Before committing run fmt, clippy and relevant tests; native platform checks
  that are unavailable locally must run in CI.
