# Python GUI consumers

Six small apps show that the `vst_audio` Python package works inside the event
loops of common Python GUI toolkits, as the [Rust GUI consumers](../gui) do for
egui, iced, Slint and Tauri. Each one opens a window with the plugin name, a
status line, details and three buttons: Open editor, Close editor and Render
test tone. The plugin editor opens in the worker process, never in the app.

| Script | Toolkit | Event loop and cross-thread call |
| --- | --- | --- |
| `ctk_host.py` | CustomTkinter | Tk `mainloop`; a 10 ms `root.after` poll drains a queue |
| `pyqt6_host.py` | PyQt6 | Qt event loop; a signal with a queued connection |
| `pyside6_host.py` | PySide6 | Same window as PyQt6 (`qt_view.py`) |
| `wx_host.py` | wxPython | wxWidgets `MainLoop`; `wx.CallAfter` |
| `gtk_host.py` | PyGObject, GTK 4 | GLib main loop; `GLib.idle_add` |
| `webview_host.py` | pywebview | Native webview; the page calls Python through `window.pywebview.api` |

`common.py` holds the part that does not depend on a toolkit. `vst_audio.Plugin`
belongs to the thread that created it and its calls wait for the worker, so a
private thread owns the plugin and the GUI never calls the binding. Each app
gives the controller a function that runs a callable on the GUI thread, and
runs a 50 ms timer that calls `heartbeat()`. The smoke run counts the ticks the
GUI thread handles while a binding call is running on the private thread; at
least one shows that the event loop kept running during the blocking call. A
fast machine can finish every call between two ticks, so the smoke renders again
until such a tick lands, and a blocked loop fails at the 45 s deadline.

CustomTkinter draws its widgets on a Tk canvas and runs on tkinter's event loop,
so it also covers plain Tk. PyGObject is the maintained successor of PyGTK. The
pywebview page (`webview_host.html`) is loaded from the file next to the script,
without a network or a build step. It calls Python for the three actions and
for its heartbeat, so a passing run also shows that the page script and the
JavaScript bridge work.

## Installing the toolkits

`requirements.txt` pins the versions that are tested. Install them next to the
`vst-audio` package:

```text
python -m pip install vst-audio -r examples/python/requirements.txt
```

On Linux, Tk, PyGObject with GTK 4 and wxPython come from the distribution,
because wxPython has no Linux wheel on PyPI and PyGObject needs the system's
introspection data. On Debian or Ubuntu that is `python3-tk`, `python3-gi`,
`gir1.2-gtk-4.0` and `python3-wxgtk4.0`. pywebview's GTK backend uses GTK 3
with WebKitGTK (`gir1.2-webkit2-4.1`). Create the virtual environment with
`--system-site-packages` so that it sees those packages. On Windows and macOS,
Tk comes with the python.org installers and pip installs the other toolkits
except GTK, which these examples use on Linux only. pywebview uses WebView2 on
Windows and WebKit on macOS.

## Running one by hand

```text
python examples/python/pyside6_host.py --plugin path/to/Plugin.vst3
```

`--plugin` takes a VST2 binary or bundle or a `.vst3` bundle. `--worker`
chooses a worker executable; without it the package uses `VST_AUDIO_WORKER` or
its bundled worker. pywebview picks its backend from the `PYWEBVIEW_GUI`
environment variable when it is set.

`--smoke` loads the plugin, opens and closes the editor, renders a test tone
and exits with 0, or 1 on the first failure, after printing one JSON summary
line. It needs a plugin that passes audio, because a silent render fails it.
`run_tests.py` runs the smoke for every app against the native VST2 and VST3
test plugins, with GTK on Linux only.

On a headless Linux machine, run the apps under Xvfb and a session bus without
services:

```text
xvfb-run -a -s "-screen 0 1280x800x24 -noreset" dbus-run-session \
  --config-file=tools/gui-session-bus.conf -- \
  env PYWEBVIEW_GUI=gtk GSK_RENDERER=cairo LIBGL_ALWAYS_SOFTWARE=1 \
  python examples/python/wx_host.py --plugin path/to/Plugin.vst3 --smoke
```

`-noreset` matters for wxPython. `wx.App` first opens and closes a test
connection to the display; when that was the server's only client, Xvfb resets
and can refuse the connection GTK opens right after it.
