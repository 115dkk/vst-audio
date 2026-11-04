# Python and .NET bindings (2026-11-04)

Accepted. vst-audio is published as the Python package `vst-audio` (PyPI,
later conda-forge) and the .NET package `VstAudio` (NuGet), beside the crates.
Both are thin layers over the `vst-audio` crate built from the same commit; they
add no hosting behaviour of their own.

## Shape

- Each package carries the native worker for its platform, so a binding never
  needs a separately installed worker and the protocol and ring versions always
  match. `Host()` without a path uses the bundled worker; an explicit worker
  path stays possible.
- The bindings live under `bindings/`, outside the Cargo workspace and outside
  the release gate's shipped files. Their Rust crates are `publish = false`.
  `cargo test --workspace` therefore needs neither Python nor .NET, and binding
  changes never release the crates. Binding packages carry their own semver,
  starting at 0.1.0 for the first functional release.
- Each binding crate has its own `Cargo.lock` with the `=<version>` pins of the
  workspace crates. `tools/release_gate.py` rewrites those locks together with
  the workspace ones on every crate version bump, so `--locked` builds keep
  working.
- A binding release is tagged `python-v<version>` or `dotnet-v<version>`. The
  crate gate's `v<version>` tags never collide with them. conda-forge builds
  from the GitHub source archive of a `python-v` tag, because the PyPI sdist
  holds the extension's dependencies but not the worker application.
- Supported targets are those of the crates: Windows x64, Linux x64 and macOS
  arm64 and x86_64. Linux wheels are `manylinux_2_28`, and the extension and
  the worker are built in the same `quay.io/pypa/manylinux_2_28_x86_64`
  container. A worker built there with Rust 1.99.0 needs only `libc`,
  `libpthread`, `libdl`, `libgcc_s` and the dynamic loader, and no glibc symbol
  newer than 2.28: the worker opens Wayland libraries with `dlopen` and x11rb
  speaks the X protocol itself. `auditwheel` checks every wheel in CI.

## Surface

The first release exposes the control and offline paths only:

- `Host(worker=None)`, `scan(path, format)` and `scan_sandboxed`;
- loading an instance with sample rate, block size and channel layout, which
  blocks the calling thread (Python releases the GIL);
- offline processing of a planar buffer, f32 and f64, with the reported
  latency (see below);
- parameters (`ListParameters`, `SetParameter`), editor open and close, state
  save and restore, recovery and shutdown;
- events polled from `try_next_event`, converted to plain values.

Offline processing must return every block. `BlockEndpoint::process` is
realtime: a block that is late is reported as `Missed` and its result is gone
(ADR 0004). The bindings therefore use a new non-realtime call in the
`vst-audio` crate that submits block `k`, waits with a deadline for block
`k - depth` to complete, and returns it; a deadline that passes, or a worker
that fails, is an error, never silence or a fallback. Recovery during an
offline render is also an error, since the replacement worker restarts at
block 0. That call lands in the crate, with its own tests, before the Python
package uses it.

The realtime variable-callback path (`AudioEndpoint`) is not exposed to Python:
a callback that holds the GIL cannot meet the realtime contract. .NET may expose
`BlockEndpoint.process` over spans later, once a test shows a callback that
neither allocates nor waits for the garbage collector.

## Python

PyO3 with maturin, `abi3-py39`, one wheel per target plus an sdist. The crate
root has `#![forbid(unsafe_code)]` like the other safe crates; PyO3 0.30's
macro output compiles under it, and slow calls release the interpreter with
`Python::detach`. Buffers cross as NumPy arrays of shape `(channels, frames)`
through the `numpy` crate, which builds and runs under `abi3-py39` (checked with
Python 3.13 and 3.14 on one abi3 wheel).

## .NET

A C ABI crate, `vst-audio-ffi-sys`, exports opaque handles, status codes and a
thread-local last-error message; every unsafe operation states its preconditions
as in the other `-sys` crates. The C# package targets `netstandard2.0` and
`net8.0`, wraps handles in `SafeHandle`, and ships the native library and worker
under `runtimes/<rid>/native/`. The same C ABI can later serve C, C++ and other
languages.

## Alternatives rejected

- Publishing the bindings at the crate version: a placeholder or an unchanged
  binding would claim releases it does not contain, and every crate release
  would force a binding release.
- One C ABI for Python too (cffi or ctypes): it puts unsafe in front of a
  language PyO3 serves safely and moves NumPy conversion into hand-written code.
- Requiring a separately installed worker: it repeats the Cargo situation the
  distribution guide warns about, where library and worker drift apart.
