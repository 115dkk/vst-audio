# .NET GUI consumers

`WpfHost` and `WinUIHost` are two small Windows apps that use the `VstAudio`
NuGet package as an application would. Each references the package, runs with
the worker the package bundles, loads a VST2 or VST3 plugin into that worker
process, opens and closes the plugin's editor (a window owned by the worker, not
by the app) and renders a test tone through the plugin. They show that the
package works under WPF and WinUI 3, as `examples/gui` does for Rust toolkits
and `examples/python` for Python toolkits, with the same behaviour as the Python
examples.

Every VstAudio call blocks until the worker answers. The apps therefore run
each call on the thread pool and update the window on the UI thread when it
returns. A 50 ms UI timer counts heartbeats in the meantime.

- `Shared/` holds the toolkit-independent part, compiled into both apps:
  argument parsing, the load, editor and render calls, the test tone, the smoke
  sequence and its JSON summary. It uses no UI types.
- `WpfHost/` is a WPF app for `net8.0-windows`.
- `WinUIHost/` is a WinUI 3 app on Windows App SDK 2.6.5, unpackaged and
  self-contained, for `win-x64`.
- `run_tests.py` builds both apps against a VstAudio package and runs their
  smoke tests with the native VST2 and VST3 fixtures.

## Platforms

Both apps run only on Windows x64. WinUIHost needs Windows 10 version 1809 or
later and the .NET 8 runtime; WpfHost needs the .NET 8 Windows Desktop runtime.

WinUIHost builds only on Windows, because the WinUI XAML compiler, `makepri`
and `mt` are Windows programs. Plain `dotnet build` is enough: neither project
needs Visual Studio or MSIX packaging. WpfHost also compiles on Linux and macOS
(`EnableWindowsTargeting`) with Microsoft's .NET SDK. Distribution builds of
the SDK, such as Ubuntu's `dotnet8` package, leave out the Windows Desktop SDK
and cannot build it.

## Build

By default, restore reads VstAudio from `target/dotnet-pack` as well as from
nuget.org, so that directory must exist. Pack the package from this checkout
(with the natives of the current platform only) or choose another source:

```sh
python3 bindings/dotnet/stage_native.py
dotnet pack bindings/dotnet/VstAudio/VstAudio.csproj -c Release -o target/dotnet-pack
dotnet build examples/dotnet/WpfHost -c Release
dotnet build examples/dotnet/WinUIHost -c Release

# A directory holding the packed VstAudio package, such as the multi-platform one from CI
dotnet build examples/dotnet/WinUIHost -c Release -p:VstAudioPackageDir=<directory>

# The published package from nuget.org alone
dotnet build examples/dotnet/WinUIHost -c Release -p:VstAudioPackageDir=
```

Each restore extracts packages into a new folder under `target/dotnet-packages/`,
so a repacked VstAudio is never replaced by an older copy of the same
version from the NuGet cache. Pass `-p:RestorePackagesPath=<directory>` to reuse
a folder instead; the WinUI restore extracts about 1.2 GB of Windows App SDK
packages.

`python3 examples/dotnet/run_tests.py [--package-dir <directory>]` builds both
apps into `target/dotnet-gui/` and runs the smoke tests (Windows only).

## Run

```text
WpfHost.exe   --plugin <path> [--worker <path>] [--smoke] [--summary <file>]
WinUIHost.exe --plugin <path> [--worker <path>] [--smoke] [--summary <file>]
```

The executables are in `WpfHost/bin/Release/net8.0-windows/` and
`WinUIHost/bin/Release/net8.0-windows10.0.19041.0/win-x64/`, or in the
directory given to `dotnet build --output`.

- `--plugin` names a VST2 `.dll` or a `.vst3` bundle.
- `--worker` names a worker executable. Without it, VstAudio uses
  `VST_AUDIO_WORKER`, then the bundled
  `<app directory>/runtimes/win-x64/native/vst-audio-worker.exe`.
- `--smoke` loads the plugin, opens and closes its editor and renders 0.5 s of
  quiet sines (one pitch per input channel). It requires that the UI timer ran
  and, for a plugin with inputs, a nonzero output peak. It then writes one JSON
  line to standard output and to the `--summary` file, and exits with 0, or
  with 1 on the first failure or after 45 s.
- Missing or unknown arguments exit with 2 after a message on standard error
  (and a message box, except in a smoke run).

Closing the window stops the plugin's worker and exits with 0. A smoke summary
looks like this:

```json
{"toolkit":"wpf","plugin":"Matrix.vst3","worker":"C:\\app\\runtimes\\win-x64\\native\\vst-audio-worker.exe","inputs":2,"outputs":6,"latency":64,"heartbeats":9,"peaks":[0.25,0.25,0.2462,0.2462,0.0625,0.0625],"exit":0}
```

A failed run adds `"error"` and leaves out the values it did not reach.

## Why WinUIHost is self-contained

`WindowsAppSDKSelfContained` copies the Windows App SDK runtime next to
`WinUIHost.exe`. The app therefore starts on a machine without the Windows App
Runtime installed and without MSIX packaging. An unpackaged framework-dependent
app would need the Windows App Runtime installer to run first. The price is a
larger output directory. .NET itself stays framework-dependent.

A self-contained Windows App SDK build needs a target architecture, so the
project sets `RuntimeIdentifier` to `win-x64`. In such a build the .NET SDK
would normally copy the package's native files flat into the output directory;
VstAudio's `buildTransitive/VstAudio.targets` keeps their package paths, so
the worker still lands at `runtimes/win-x64/native/vst-audio-worker.exe`.
