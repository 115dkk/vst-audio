# VstAudio

.NET 8 bindings for [vst-audio](https://github.com/115dkk/vst-audio), a Rust
library that hosts VST2 and VST3 plugins in separate worker processes.
The package includes the native C ABI library and matching worker.

## Install

```sh
dotnet add package VstAudio --version 0.2.0
```

```csharp
using VstAudio;

using var host = new Host();
using var plugin = host.Load("/path/to/effect.vst3",
    options: new LoadOptions { SampleRate = 48000, BlockSize = 128 });

// One array per input channel, each with the same number of frames.
float[][] input = Enumerable.Range(0, plugin.InputChannels)
    .Select(_ => new float[1061]).ToArray();
float[][] output = plugin.Process(input, tail: 193);
// output[channel] has 1254 samples; the plugin's reported latency is compensated.
// Dispose stops the worker; the using declarations dispose both objects.
```

## Supported operations

Version 0.2.0 exposes scanning (including sandboxed scanning), class selection,
synchronous loading, offline Float32/Float64 rendering, parameter queries and
updates, state save/restore, and worker editor open/close. Float64 requires
native plugin support. Processing accepts jagged channel arrays or flat planar
spans; input and output spans must not overlap. For instruments with no inputs,
use the span overload to specify the number of frames. 0.2.0 adds the experimental
realtime block path below and moves to the MIT OR Apache-2.0 licence; its native
library speaks C ABI version 2, so 0.1.0 natives and 0.2.0 managed code do not mix.

Calls block the caller and are serialized per instance; in a GUI, run them with
`await Task.Run(...)` so the UI thread stays free, as the
[WPF and WinUI 3 examples](https://github.com/115dkk/vst-audio/tree/main/examples/dotnet) do. Control and per-block
render deadlines default to 10 seconds; loading defaults to 30 seconds. Negative
timeouts are rejected; values above `uint.MaxValue` milliseconds are clamped.
Rendering fails with an exception on a missed deadline, lost worker or recovery;
it never substitutes silence for missing results. Dispose is idempotent and can
run on any thread. A plugin remains usable after its host has been disposed.

## Experimental realtime block path

Set `LoadOptions.Realtime = true`, then call `Plugin.TakeBlockProcessor()` once.
The returned experimental `BlockProcessor` accepts flat planar spans containing
exactly `channels * Quantum` samples and returns `Delivered`, `Missed`, or
`InvalidBuffer` without waiting for a late worker. Taking it disables the plugin's
offline `Process` methods. This API is marked `VSTA0001` (ADR 0008). The
package's tests show that a call allocates nothing and never waits for the
worker, but also that a managed audio thread stops for every blocking garbage
collection in the process. Keep blocking collections out of the process while
audio plays, or call the C ABI's `vsta_blocks_process_f32` from a native audio
thread.

The control/offline API follows ADR 0006. Event polling, explicit recovery commands, channel-layout selection and
cross-architecture worker registration are not exposed by this .NET release.
.NET Framework and Unity are not supported.

## Native platforms and worker lookup

- Windows x64 (`win-x64`)
- Linux x64, glibc 2.28 or newer (`linux-x64`)
- macOS Apple Silicon (`osx-arm64`)
- macOS Intel (`osx-x64`)

Native assets and dependency notices live in `runtimes/<rid>/native/`.
The package's MSBuild targets preserve subdirectories in build and publish
outputs, including `VstAudioWorker.app/Contents/MacOS/vst-audio-worker` on macOS.
The helper has an ad-hoc signature, not Developer ID signing or notarization;
application distributors must sign and notarize their final distribution.

`new Host()` checks `VST_AUDIO_WORKER`, then the application's
`runtimes/<rid>/native/` directory, then the application directory. You can pass
an explicit executable path to `new Host(workerPath)`. A missing explicit path
throws `WorkerNotFoundException`. Use a worker from the same native release.
On Unix, the package adds execute permission for each existing read permission
on a bundled worker, because NuGet does not preserve file modes. It never
changes permissions on explicit paths or paths from the environment variable.
The application must have permission to update the bundled file mode on first
use (or the distributor must set its execute permissions before deployment).

Licensed under MIT OR Apache-2.0, at your option. The bundled worker's
notices, including its BSD-3-Clause and ISC parts, ship with the package.

VST is a registered trademark of Steinberg Media Technologies GmbH.
This project is independent of Steinberg and is not endorsed by it.
