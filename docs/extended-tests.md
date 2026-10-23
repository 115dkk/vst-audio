# Extended verification

The native matrix runs a repeated recovery test for each format. It varies
callback lengths through 1, 7, 37, 83, 129 and 256 samples, injects four worker
crashes, and requires four complete generation changes while a second plugin
continues producing numerically correct samples. At least 400 healthy callbacks
per format must pass, and at least 90% of measured healthy-peer callbacks must be
processed rather than use fallback. A separate native test runs real-time-paced
480-frame callbacks over a 128-frame quantum after warm-up, requires at least 95%
processed callbacks with numeric output, and verifies that 481 frames are refused
without disturbing following calls. The callback allocator guard covers both
instances, including endpoint handover and retirement. The wall-clock deadline
bounds hangs; hosted scheduling latency is not a hardware audio benchmark.

`python3 tools/fuzz_tests.py` runs two coverage-guided libFuzzer targets for
120 seconds each, using cargo-fuzz 0.13.2 and nightly-2026-10-20:

- `control`: framed control parsing, streaming reads, canonical re-encoding and
  VST3 state-envelope bounds.
- `ring`: mutated shared-memory headers and event packets, including mutation
  after geometry validation, with safe atomic storage.

Crashing public-input cases are retained under `fuzz/artifacts`; reports record
the actual execution count. Fuzzing time is configurable with
`VST_AUDIO_FUZZ_SECONDS`. A bounded successful run is evidence for those inputs,
not a proof that all possible malformed inputs are safe.

`python3 tools/asan_tests.py` builds the worker, fixture libraries and native
tests with AddressSanitizer and leak detection on Linux. The sanitizer runtime
is exported from the worker so instrumented `dlopen` plugins resolve the same
runtime. Normal native tests, crash/hang scenarios, editor operations, f64/MIDI,
recovery and stress all execute with instrumentation. Any sanitizer finding
fails the job, including a finding from a worker whose death the isolation test
otherwise expected. External plugin binaries are not sanitizer artifacts.

Both lanes run on every CI push and pull request. The regular four-target jobs
continue to compile and execute with the consumer's stable Rust toolchain.
