# MIT OR Apache-2.0 (2026-11-05)

Accepted. From the release after 0.11.0 the crates, the worker and the Python
and .NET bindings are offered under the MIT license or the Apache License,
Version 2.0, at the user's option. Earlier releases stay MIT only.

## Why

- It is the Rust ecosystem's default. The project's own dependencies (vst3-rs,
  PyO3, the numpy crate) use the same pair, and license policies such as
  `cargo deny` configurations usually allow it as is.
- Apache-2.0 adds an explicit patent license with a termination clause and
  states that contributions come under the same terms (section 5). Companies
  that embed a plugin host in a product, such as a game or a DAW, often
  require one of those.
- MIT stays available, so nobody loses a right they had: GPL-2.0 projects,
  which cannot take Apache-2.0 code, keep using the MIT option.

## Rights

The copyright of the project's own code is held by 115dkk, who made this
decision. Parts with their own terms keep them and are compatible with both
options:

- `crates/vst-audio-worker-sys/src/vst2.rs` stays BSD-3-Clause and the DPF
  layouts ISC, so that crate's expression is
  `(MIT OR Apache-2.0) AND BSD-3-Clause AND ISC`.
- The StreamRing derivative is the author's own code, which the author
  permits under both licenses.
- cyroz1's patches were received under MIT. MIT permits sublicensing, so the
  adapted parts are offered under the project's terms with their attribution
  kept in `NOTICE`. That grants no patent license from cyroz1.

## Consequences

- The root and every published crate carry `LICENSE-MIT` and
  `LICENSE-APACHE`; `tools/package_check.py` rejects a crate archive without
  either, and the worker ZIPs, wheels and NuGet natives ship both.
- Under the Apache option, redistributors must keep the attribution notices in
  `NOTICE` (Apache-2.0 section 4(d)).
