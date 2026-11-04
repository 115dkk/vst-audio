"""Isolated VST2/VST3 hosting and synchronous offline audio rendering."""
from __future__ import annotations

from importlib.metadata import version
import os
from pathlib import Path
import sys
from typing import Literal

from ._native import (
    Host as _Host,
    Parameter,
    Plugin,
    PluginClass,
    RenderError,
    RequestTimeout,
    StartError,
    VstAudioError,
    WorkerNotFoundError,
)

__version__ = version("vst-audio")
__all__ = [
    "Host", "Plugin", "PluginClass", "Parameter", "VstAudioError",
    "WorkerNotFoundError", "StartError", "RenderError", "RequestTimeout",
]


def _bundled_worker() -> Path:
    folder = Path(__file__).parent / "_worker"
    if sys.platform == "darwin":
        return folder / "VstAudioWorker.app/Contents/MacOS/vst-audio-worker"
    return folder / ("vst-audio-worker.exe" if sys.platform == "win32" else "vst-audio-worker")


def _format(path: Path, format: str | None) -> str:
    if format is None:
        format = {".vst3": "vst3", ".dll": "vst2", ".so": "vst2", ".vst": "vst2"}.get(path.suffix.lower())
    if format not in ("vst2", "vst3"):
        raise ValueError("format must be 'vst2' or 'vst3'; cannot infer it from this path")
    return format


class Host:
    """Locate a worker and scan or load plugins without loading foreign code here."""

    def __init__(self, worker: str | os.PathLike | None = None) -> None:
        bundled = _bundled_worker()
        if worker is not None:
            selected = Path(worker).expanduser()
        else:
            configured = os.environ.get("VST_AUDIO_WORKER")
            selected = Path(configured).expanduser() if configured else bundled
        if not selected.is_file():
            raise WorkerNotFoundError(
                f"Worker not found at {selected}. Set VST_AUDIO_WORKER to a worker executable "
                f"or install the bundled worker at {bundled}."
            )
        self._worker = selected.resolve()
        self._host = _Host(self._worker)

    @property
    def worker(self) -> Path:
        """The resolved worker executable."""
        return self._worker

    def scan(self, path: str | os.PathLike, format: Literal["vst2", "vst3"] | None = None,
             *, sandboxed: bool = False) -> list[PluginClass]:
        """Scan classes in a disposable worker, optionally with OS restrictions."""
        path = Path(path).expanduser().resolve()
        return self._host.scan(path, _format(path, format), sandboxed)

    def load(self, path: str | os.PathLike, format: Literal["vst2", "vst3"] | None = None,
             *, class_id: bytes | None = None, sample_rate: int = 48000,
             block_size: int = 512, sample_format: Literal["f32", "f64"] = "f32",
             timeout: float = 30.0) -> Plugin:
        """Load one class in an isolated worker configured for offline processing."""
        path = Path(path).expanduser().resolve()
        return self._host.load(path, _format(path, format), class_id, sample_rate,
                               block_size, sample_format, timeout)
