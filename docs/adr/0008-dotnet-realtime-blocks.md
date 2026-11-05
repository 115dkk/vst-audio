# Realtime blocks in .NET (2026-11-05)

Accepted as experimental. ADR 0006 deferred `BlockEndpoint::process` in .NET
"once a test shows a callback that neither allocates nor waits for the garbage
collector". That test now exists, and its answer is split.

## What the C ABI and the package add

- `vsta_host_load` takes a `realtime` flag (the plugin is configured with
  `offline = false`); the ABI version is 2.
- `vsta_plugin_take_blocks` moves the instance's `BlockEndpoint` into a
  `VstaBlocks` handle. `vsta_blocks_process_f32` and `_f64` submit block `k`
  and return block `k - depth` as Delivered, Missed or InvalidBuffer, without
  allocating, locking, waiting or touching the last-error state. After the
  move, offline rendering fails with the kind `realtime_owned`.
- In C#, `LoadOptions.Realtime`, `Plugin.TakeBlockProcessor()` and
  `BlockProcessor.Process(ReadOnlySpan<T>, Span<T>)`, all marked
  `[Experimental("VSTA0001")]`. A `SafeHandle` keeps a running call alive if
  another thread disposes the processor.

## Evidence

`bindings/ffi/tests/no_alloc.rs` runs 2,000 realtime calls, Delivered and
Missed, under `assert_no_alloc`. `bindings/dotnet/VstAudio.Tests/RealtimeTests.cs`
runs in the `dotnet-test` CI job on all four targets and prints one JSON line
per measurement. The first measurements, Linux x64 with .NET 8, three runs:

| Test | Result |
|---|---|
| 20,000 calls as fast as possible | 0 bytes allocated on the calling thread |
| Worker hung inside its process call | every call Missed; slowest 0.037 to 0.040 ms |
| Blocking compacting gen2 GC from another thread, ~200 MB live | audio thread's longest gap 72.8, 78.2, 88.4 ms; longest GC pause 72.6, 78.1, 88.3 ms |
| `SustainedLowLatency`, short-lived garbage from another thread | longest gap 8.4 to 9.4 ms (baseline without load 1.6 to 5.0 ms); 0 bytes allocated |

## Decision

- The call allocates nothing and never waits for the worker: proven.
- A managed callback that never waits for the garbage collector is not
  possible. The CLR suspends every thread running managed code for a blocking
  collection, whoever triggered it, and the gaps above match the pauses. No
  API shape changes that.
- `BlockProcessor` therefore ships as experimental rather than as the general
  realtime API ADR 0006 described. A .NET application that uses it must keep
  blocking collections out of its process during playback (no steady
  allocation, `GCSettings.LatencyMode`, `GC.TryStartNoGCRegion`), or call
  `vsta_blocks_process_f32` from a native audio thread. The CLR does not
  suspend threads that never run managed code; that path is documented CLR
  behaviour and is not tested here.
- Unity's managed `OnAudioFilterRead` runs under a stop-the-world collector as
  well, so a GC-independent Unity integration would be a native audio plugin
  calling the C ABI. Neither is built or tested here: Unity needs an
  activated licence in CI.
