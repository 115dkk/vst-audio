"""Toolkit-independent controller shared by the Python GUI consumer examples.

``vst_audio.Plugin`` is bound to the thread that created it, and its calls
block until the worker answers. One private thread therefore owns the host and
the plugin, and a GUI never calls the binding directly. The GUI hands the
controller a ``post`` function that runs a callable on the GUI thread and a
view with two methods; the controller never touches widgets.

``--smoke`` runs load, open editor, close editor and one render without input,
then leaves the event loop with exit code 0, or 1 on the first failure. It also
requires that the GUI timer called :meth:`Controller.heartbeat` while the
binding was busy, which shows that the event loop kept running.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass, replace
import json
import math
from pathlib import Path
import queue
import sys
import threading
import time
from typing import Any, Callable, Optional, Protocol

import numpy as np
import vst_audio

SAMPLE_RATE = 48000
BLOCK_SIZE = 256
# A smoke run that has not finished by then fails, even if every reply is slow
# but still arriving. tools and CI give the process a longer outer timeout.
SMOKE_DEADLINE = 45.0
HEARTBEAT_MS = 50


@dataclass(frozen=True)
class Options:
    plugin: Path
    worker: Optional[Path]
    smoke: bool


@dataclass(frozen=True)
class State:
    """Everything a view draws. Views redraw from this snapshot only."""

    plugin: str
    status: str
    details: str = ""
    # A plugin is loaded and no binding call is in flight.
    ready: bool = False
    editor_open: bool = False
    failed: bool = False


class View(Protocol):
    def refresh(self, state: State) -> None:
        """Redraw labels and button sensitivity. Called on the GUI thread."""

    def finish(self, code: int) -> None:
        """Smoke runs only: leave the event loop so the process exits with ``code``."""


def parse_args(toolkit: str, argv: Optional[list] = None) -> Options:
    parser = argparse.ArgumentParser(
        description=f"vst-audio {toolkit} consumer: a plugin in its own worker process")
    parser.add_argument("--plugin", type=Path, required=True,
                        help="VST2 binary or bundle, or a .vst3 bundle")
    parser.add_argument("--worker", type=Path,
                        help="worker executable; defaults to VST_AUDIO_WORKER or the bundled worker")
    parser.add_argument("--smoke", action="store_true",
                        help="run the automatic lifecycle check and exit")
    args = parser.parse_args(argv)
    return Options(args.plugin.expanduser().resolve(), args.worker, args.smoke)


def test_tone(channels: int, frames: int, sample_rate: int) -> np.ndarray:
    """Quiet sines, one pitch per channel, shaped ``(channels, frames)``."""
    time_axis = np.arange(frames, dtype=np.float64) / sample_rate
    tones = [0.25 * np.sin(2 * np.pi * (440.0 + 110.0 * channel) * time_axis)
             for channel in range(channels)]
    return np.array(tones, dtype=np.float32).reshape(channels, frames)


class Controller:
    """Owns one plugin on a private thread and reports to a view on the GUI thread."""

    def __init__(self, toolkit: str, options: Options, view: View,
                 post: Callable[[Callable[[], None]], None]) -> None:
        self.toolkit = toolkit
        self.options = options
        self._view = view
        self._post = post
        self._lock = threading.RLock()
        self._jobs: "queue.SimpleQueue[Optional[Callable[[], None]]]" = queue.SimpleQueue()
        self._thread = threading.Thread(target=self._serve, name="vst-audio session", daemon=True)
        self._plugin: Any = None  # touched on the session thread only
        self._state = State(plugin=options.plugin.name, status="Opening plugin…")
        self._heartbeats = 0
        self._summary: dict = {"toolkit": toolkit, "plugin": options.plugin.name}
        self._finished = False
        self._watchdog: Optional[threading.Timer] = None

    # GUI-facing API ---------------------------------------------------------

    @property
    def state(self) -> State:
        with self._lock:
            return self._state

    def start(self) -> None:
        """Start the session thread and the load. Call once the window exists."""
        if self.options.smoke:
            self._watchdog = threading.Timer(SMOKE_DEADLINE, lambda: self._post(
                lambda: self._fail("smoke", TimeoutError(f"no result within {SMOKE_DEADLINE:.0f} s"))))
            self._watchdog.daemon = True
            self._watchdog.start()
        self._thread.start()
        self._refresh()
        self._submit("load", self._load, self._loaded)

    def heartbeat(self) -> None:
        """Call from a GUI timer every :data:`HEARTBEAT_MS` milliseconds."""
        with self._lock:
            self._heartbeats += 1

    def open_editor(self) -> None:
        if self._begin("Opening editor…"):
            self._submit("open editor", lambda: self._plugin.open_editor(), self._editor_opened)

    def close_editor(self) -> None:
        if self._begin("Closing editor…"):
            self._submit("close editor", lambda: self._plugin.close_editor(), self._editor_closed)

    def render(self) -> None:
        if self._begin("Rendering a test tone…"):
            self._submit("render", self._render, self._rendered)

    def shutdown(self, timeout: float = 15.0) -> None:
        """Stop the worker and the session thread. Safe to call more than once."""
        if self._watchdog is not None:
            self._watchdog.cancel()
        if self._thread.is_alive():
            self._jobs.put(self._close)
            self._jobs.put(None)
            self._thread.join(timeout)

    # Session thread ---------------------------------------------------------

    def _serve(self) -> None:
        while True:
            job = self._jobs.get()
            if job is None:
                return
            job()

    def _submit(self, label: str, work: Callable[[], Any],
                done: Callable[[Any], None]) -> None:
        def job() -> None:
            try:
                result = work()
            except Exception as error:  # every binding error is reported, never hidden
                # Python unbinds ``error`` when the except block ends, before the
                # GUI thread runs the callable, so the default argument keeps it.
                self._post(lambda failure=error: self._fail(label, failure))
            else:
                self._post(lambda: done(result))
        self._jobs.put(job)

    def _load(self) -> dict:
        started = time.perf_counter()
        host = vst_audio.Host(self.options.worker)
        plugin = host.load(self.options.plugin, sample_rate=SAMPLE_RATE, block_size=BLOCK_SIZE)
        self._plugin = plugin
        return {"worker": str(host.worker), "inputs": plugin.input_channels,
                "outputs": plugin.output_channels, "latency": plugin.latency,
                "sample_rate": plugin.sample_rate, "block_size": plugin.block_size,
                "load_seconds": round(time.perf_counter() - started, 3)}

    def _render(self) -> dict:
        plugin = self._plugin
        frames = plugin.sample_rate // 2
        audio = test_tone(plugin.input_channels, frames, plugin.sample_rate)
        started = time.perf_counter()
        output = plugin.process(audio)
        seconds = time.perf_counter() - started
        expected = (plugin.output_channels, frames)
        if output.shape != expected:
            raise RuntimeError(f"render returned shape {output.shape}, expected {expected}")
        if not np.isfinite(output).all():
            raise RuntimeError("render returned a non-finite sample")
        peaks = [round(float(np.max(np.abs(channel))), 4) for channel in output]
        return {"frames": frames, "render_seconds": round(seconds, 3), "peaks": peaks}

    def _close(self) -> None:
        plugin, self._plugin = self._plugin, None
        if plugin is not None:
            plugin.close()

    # GUI thread -------------------------------------------------------------

    def _begin(self, status: str) -> bool:
        with self._lock:
            if not self._state.ready:
                return False
            self._state = replace(self._state, status=status, ready=False)
        self._refresh()
        return True

    def _loaded(self, info: dict) -> None:
        self._summary.update(info)
        details = (f"{info['inputs']} in, {info['outputs']} out, latency {info['latency']} frames\n"
                   f"{info['sample_rate']} Hz, {info['block_size']}-frame blocks\n"
                   f"Worker: {info['worker']}")
        self._update(status="Ready", details=details, ready=True)
        if self.options.smoke:
            self.open_editor()

    def _editor_opened(self, _: Any) -> None:
        self._update(status="Editor open in the worker process", ready=True, editor_open=True)
        if self.options.smoke:
            self.close_editor()

    def _editor_closed(self, _: Any) -> None:
        self._update(status="Editor closed", ready=True, editor_open=False)
        if self.options.smoke:
            self.render()

    def _rendered(self, result: dict) -> None:
        self._summary.update(result)
        peaks = ", ".join(f"{peak:.3f}" for peak in result["peaks"])
        with self._lock:
            details = self._state.details.split("\nLast render:", 1)[0]
        self._update(status="Rendered", ready=True,
                     details=f"{details}\nLast render: {result['frames']} frames in "
                             f"{result['render_seconds']:.3f} s, output peaks {peaks}")
        if self.options.smoke:
            self._check_smoke(result)

    def _check_smoke(self, result: dict) -> None:
        with self._lock:
            beats = self._heartbeats
        self._summary["heartbeats"] = beats
        if beats == 0:
            self._fail("smoke", RuntimeError("the GUI timer never ran while the plugin was busy"))
        elif self._summary["inputs"] and not any(peak > 0 and math.isfinite(peak)
                                                 for peak in result["peaks"]):
            self._fail("smoke", RuntimeError("the plugin returned silence for a test tone"))
        else:
            self._finish(0)

    def _fail(self, label: str, error: BaseException) -> None:
        print(f"{self.toolkit}: {label} failed: {type(error).__name__}: {error}",
              file=sys.stderr, flush=True)
        with self._lock:
            loaded = label != "load" and self._plugin is not None
        if label == "load":
            self._update(status="The plugin could not be opened. Check its path and restart.",
                         details=f"{type(error).__name__}: {error}", failed=True)
        else:
            self._update(status=f"Could not {label}.", ready=loaded,
                         details=f"{type(error).__name__}: {error}")
        if self.options.smoke:
            self._summary["error"] = f"{label}: {error}"
            self._finish(1)

    def _finish(self, code: int) -> None:
        with self._lock:
            if self._finished:
                return
            self._finished = True
        if self._watchdog is not None:
            self._watchdog.cancel()
        self._summary["exit"] = code
        print(json.dumps(self._summary), flush=True)
        self._view.finish(code)

    def _update(self, **changes: Any) -> None:
        with self._lock:
            self._state = replace(self._state, **changes)
        self._refresh()

    def _refresh(self) -> None:
        self._view.refresh(self.state)
