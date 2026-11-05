# vst-audio

Python bindings for [vst-audio](https://github.com/115dkk/vst-audio), a Rust
library that hosts VST2 and VST3 plugins in separate worker processes.
Scanning also runs outside Python. No audio device is opened.

## Installation

```sh
pip install vst-audio
```

Requires Python 3.9 or newer and NumPy. Functional native wheels contain a
worker for their platform; the earlier 0.0.1 package was a name reservation
and does not host plugins. This implementation targets Windows, Linux and
macOS. Native execution has currently been verified on Windows x64 only.
Use plugins built for the same architecture as the packaged worker.

```python
import numpy as np, vst_audio
host = vst_audio.Host()
with host.load("Plugin.vst3", sample_rate=48000, block_size=512) as plugin:
    out = plugin.process(np.zeros((plugin.input_channels, 48000), np.float32))
```

`Host()` first checks `VST_AUDIO_WORKER`, then the package's `_worker/`
directory. An invalid explicit environment override raises `WorkerNotFoundError`
rather than silently running a different worker. You can also pass an executable
path directly with `Host(worker=...)`. Plugins are never bundled.

## Supported operations

- Scan VST2 or VST3 classes, optionally in an OS sandbox (`sandboxed=True`).
- Load one plugin per worker with a sample rate, block size and f32/f64 precision.
  The plugin must support the requested precision; there is no implicit conversion.
- Select a VST3 class using `class_id=host.scan(path)[index].id`.
- Render planar NumPy arrays, including C-order and Fortran-order arrays, and
  request a silent input tail with `process(audio, tail=frames)`.
- List immutable parameter snapshots, set normalized values, and save or restore
  opaque state bytes.
- Open and close a worker-owned floating editor. A usable desktop session is
  required; opening an editor does not start audio playback.
- Close explicitly or use a context manager. `close()` is idempotent and requests
  asynchronous worker teardown. Do not rely on garbage collection for cleanup.

The input has shape `(input_channels, frames)` and dtype `numpy.float32` for
`sample_format="f32"` or `numpy.float64` for `"f64"`. The returned array has shape
`(output_channels, frames + tail)` and the same dtype. The renderer zero-pads the
last block and removes pipeline and reported plugin latency. Priming has no
samples, but counts as pipeline time so it is not trimmed twice. A zero-length
input with `tail=0` returns an empty output without processing a block.

Rendering releases the GIL and waits losslessly for each block, with `timeout`
in seconds **per block**. Control methods also release the GIL while waiting;
parameter enumeration uses one timeout for all pages. The input is copied before
releasing the GIL, so another Python thread cannot change the render in progress.
Block buffers are reused throughout the render. Each `process` call flushes
silence through the latency and rounds to whole blocks; it is an offline render,
not a gapless streaming callback. Plugin state is retained between calls.
Use a fresh instance for independent renders that require fresh DSP state.

`Plugin.latency` reports the plugin's latency in frames, excluding the pipeline.
The endpoint refreshes this value at block boundaries. Latency is sampled at the
first block of each render; plugins that change latency during a render are not
supported for sample-accurate alignment. Use a plugin only on its creating thread.

## Errors

All hosting errors derive from `VstAudioError`:

- `WorkerNotFoundError`: no worker at the configured or bundled location.
- `StartError`: startup or request submission failed. `kind` is the snake_case
  Rust `StartErrorKind` name. An expired load timeout cancels startup and has
  `kind="cancelled"`.
- `RenderError`: `kind` is `invalid_buffer`, `not_running`, `recovered`,
  `timed_out`, or `lost`. No partial render is returned.
- `RequestTimeout`: a matching control reply did not arrive in time.

A worker-reported control rejection raises `VstAudioError` with its diagnostic.
Wrong dtype raises `TypeError`; wrong array shape or invalid format raises
`ValueError`. Unrelated control events are retained internally, but this first
API does not expose an event stream. After a failed render, close the instance
and load again rather than treating the failed call as a completed render.

## Not exposed in this first API

No realtime audio-device callback, MIDI/SysEx, sample-offset automation,
transport control, bus configuration, editor embedding, recovery control,
async Python API or cross-architecture worker registry is provided. Floating
editor support comes from the Rust worker, not a Python GUI toolkit.

## Building and testing from the repository

Build tooling requires Rust, Python 3.11+ and platform-native build dependencies.
The installed package itself supports Python 3.9+. From `bindings/python`:

```sh
python stage_worker.py
python -m pip install .
python run_tests.py
```

`stage_worker.py` builds a locked release worker and stages its redistribution
notices. `--worker PATH` uses an existing executable (or a macOS `.app` bundle)
instead. On macOS the helper bundle receives an ad-hoc hardened-runtime signature;
distributors remain responsible for signing and notarization.

`run_tests.py` builds the repository fixtures, stages a release worker, creates
a fresh `target/python-venv`, builds and installs a wheel, then runs pytest against
that installed wheel. It verifies bundled-worker resolution as well as the
environment override. The Matrix fixtures verify exact f32 matrix output and
64-frame latency compensation. Their f64 rejection is intentional; separate
advanced fixtures verify f64 synthesis silence and a nonzero f64 VST3 effect.
Linux and macOS execution, editor interaction, Python 3.9 and multi-page native
parameter enumeration still need platform or fixture coverage.

Licensed under MIT OR Apache-2.0, at your option. The bundled worker's
notices, including its BSD-3-Clause and ISC parts, ship with the package.

VST is a registered trademark of Steinberg Media Technologies GmbH.
This project is independent of Steinberg and is not endorsed by it.
