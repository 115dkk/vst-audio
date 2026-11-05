"""pywebview consumer: the vst-audio binding behind an HTML page and its JS bridge."""
import dataclasses
import json
import os
from pathlib import Path
import queue
import sys
import threading
import traceback
from typing import Callable, Optional

import webview

import common

TOOLKIT = "pywebview"
PAGE = Path(__file__).with_name("webview_host.html")


class Api:
    """What the page calls as ``window.pywebview.api``. Each method returns at once.

    pywebview runs every call on a fresh thread and exposes public attributes, so
    everything else stays private.
    """

    def __init__(self, controller: common.Controller,
                 post: Callable[[Callable[[], None]], None]) -> None:
        self._controller = controller
        self._post = post

    def heartbeat(self) -> None:
        self._controller.heartbeat()

    def open_editor(self) -> None:
        self._post(self._controller.open_editor)

    def close_editor(self) -> None:
        self._post(self._controller.close_editor)

    def render(self) -> None:
        self._post(self._controller.render)


class Window:
    """The webview window and the view the controller draws through.

    pywebview has no GUI-thread call of its own: its window methods may be used
    from any thread, and ``evaluate_js`` waits for the page. One dispatcher
    thread therefore stands in for the GUI thread. It runs every posted call in
    order, so state pushes cannot overtake each other, and neither the session
    thread nor the page's API calls wait on the page.
    """

    def __init__(self, options: common.Options) -> None:
        self.code = 0
        self._closed = threading.Event()
        self._started = False
        self._posted: "queue.SimpleQueue[Optional[Callable[[], None]]]" = queue.SimpleQueue()
        self._dispatcher = threading.Thread(target=self._dispatch, name="pywebview dispatcher",
                                            daemon=True)
        self.controller = common.Controller(TOOLKIT, options, self, self._posted.put)
        page = PAGE.read_text(encoding="utf-8").replace(
            "__HEARTBEAT_MS__", str(common.HEARTBEAT_MS))
        self.window = webview.create_window(f"vst-audio · {TOOLKIT}", html=page,
                                            js_api=Api(self.controller, self._posted.put),
                                            width=600, height=300)
        self.window.events.loaded += self._on_loaded
        self.window.events.closing += self._closed.set
        if sys.platform == "darwin":
            self.window.events.closed += _wake_cocoa_loop

    def refresh(self, state: common.State) -> None:
        if not self._closed.is_set():
            self.window.evaluate_js(f"showState({json.dumps(dataclasses.asdict(state))})")

    def finish(self, code: int) -> None:
        self.code = code
        self.window.evaluate_js("stopHeartbeat()")
        self._closed.set()
        self.window.destroy()

    def run(self) -> int:
        self._dispatcher.start()
        webview.start()  # the backend comes from PYWEBVIEW_GUI or the platform default
        self.controller.shutdown()
        self._posted.put(None)
        self._dispatcher.join(5)
        _leave_stranded_bridge_calls(self.code)
        return self.code

    def _on_loaded(self) -> None:
        if not self._started:  # a reload fires the event again
            self._started = True
            self._posted.put(self.controller.start)

    def _dispatch(self) -> None:
        while True:
            function = self._posted.get()
            if function is None:
                return
            try:
                function()
            except Exception:  # report like a GUI toolkit does, and keep serving
                traceback.print_exc()


def _leave_stranded_bridge_calls(code: int) -> None:
    """Exit without the JS bridge threads a closed page can strand.

    pywebview runs each call from the page on a new non-daemon thread, which
    hands the result back with ``evaluate_js``. On Cocoa that call waits,
    without a timeout, for the main loop to run the script; once the window is
    gone it never does, and Python's exit would wait for the thread forever. A
    heartbeat in flight when the window closes is enough.
    """
    stranded = [thread for thread in threading.enumerate()
                if thread is not threading.main_thread() and not thread.daemon]
    for thread in stranded:
        thread.join(1)
    stranded = [thread for thread in stranded if thread.is_alive()]
    if stranded:
        print(f"{TOOLKIT}: leaving {len(stranded)} pywebview bridge thread(s) that wait "
              "for the closed page", file=sys.stderr, flush=True)
        sys.stdout.flush()
        os._exit(code)


def _wake_cocoa_loop() -> None:
    """Let ``webview.start()`` return after a window closed by code.

    pywebview's Cocoa backend calls ``NSApp.stop_`` when the last window
    closes, and AppKit leaves its run loop only after the next event. A user's
    click supplies one; ``window.destroy()`` may not, and the loop then waits
    forever. An application-defined event posted after the stop wakes it.
    """
    import AppKit
    from PyObjCTools import AppHelper

    def post() -> None:
        kind = getattr(AppKit, "NSEventTypeApplicationDefined", None) or AppKit.NSApplicationDefined
        event = AppKit.NSEvent.otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2_(
            kind, AppKit.NSMakePoint(0, 0), 0, 0.0, 0, None, 0, 0, 0)
        AppKit.NSApp.postEvent_atStart_(event, True)

    # Queued on the main thread, so it runs after windowWillClose_ has called stop_.
    AppHelper.callAfter(post)


def main() -> None:
    options = common.parse_args(TOOLKIT)
    sys.exit(Window(options).run())


if __name__ == "__main__":
    main()
