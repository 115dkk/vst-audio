"""PyGObject consumer: the vst-audio binding driven from the GTK 4 main loop."""
import sys
from typing import Callable, Optional

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gio, GLib, Gtk, Pango  # noqa: E402  (after require_version)

import common  # noqa: E402

TOOLKIT = "GTK 4 (PyGObject)"


def post(function: Callable[[], None]) -> None:
    """Run ``function`` once on the main loop; safe from any thread."""
    def run() -> bool:
        function()
        return GLib.SOURCE_REMOVE
    GLib.idle_add(run)


class Window:
    """The application window and the view the controller draws through."""

    def __init__(self, app: Gtk.Application, options: common.Options) -> None:
        self.code = 0
        self._app = app
        self._closed = False
        self.controller = common.Controller(TOOLKIT, options, self, post)
        self.window = Gtk.ApplicationWindow(application=app, title=f"vst-audio · {TOOLKIT}",
                                            default_width=560)
        self.window.connect("close-request", self._on_close_request)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12, margin_top=16,
                      margin_bottom=16, margin_start=16, margin_end=16)
        self._name = Gtk.Label(xalign=0)
        self._status = Gtk.Label(xalign=0, wrap=True)
        self._details = Gtk.Label(xalign=0, wrap=True, wrap_mode=Pango.WrapMode.WORD_CHAR)
        for label in (self._name, self._status, self._details):
            box.append(label)
        row = Gtk.Box(spacing=8)
        self._buttons = []
        for text, action in (("Open editor", self.controller.open_editor),
                             ("Close editor", self.controller.close_editor),
                             ("Render test tone", self.controller.render)):
            button = Gtk.Button(label=text, sensitive=False)
            button.connect("clicked", lambda _button, action=action: action())
            row.append(button)
            self._buttons.append(button)
        box.append(row)
        self.window.set_child(box)
        GLib.timeout_add(common.HEARTBEAT_MS, self._beat)

    def refresh(self, state: common.State) -> None:
        if self._closed:  # a call queued before the window closed
            return
        self._name.set_label(state.plugin)
        self._status.set_label(state.status)
        self._details.set_label(state.details)
        for button in self._buttons:
            button.set_sensitive(state.ready)

    def finish(self, code: int) -> None:
        self.code = code
        self._app.quit()

    def _beat(self) -> bool:
        self.controller.heartbeat()
        return GLib.SOURCE_CONTINUE

    def _on_close_request(self, _window: Gtk.Window) -> bool:
        self._closed = True
        return False  # let it close; the application quits with its last window


class Application(Gtk.Application):
    def __init__(self, options: common.Options) -> None:
        # NON_UNIQUE: parallel runs must not hand their activation to each other over D-Bus.
        super().__init__(application_id="io.github._115dkk.VstAudio.GtkExample",
                         flags=Gio.ApplicationFlags.NON_UNIQUE)
        self.options = options
        self.view: Optional[Window] = None

    def do_activate(self) -> None:
        if self.view is None:  # NON_UNIQUE: activated once per process
            self.view = Window(self, self.options)
            self.view.window.present()
            self.view.controller.start()


def main() -> None:
    options = common.parse_args(TOOLKIT)
    app = Application(options)
    # GTK gets the program name only; the options above are ours.
    status = app.run(sys.argv[:1])
    if app.view is not None:
        app.view.controller.shutdown()
        status = status or app.view.code
    sys.exit(status)


if __name__ == "__main__":
    main()
