"""CustomTkinter consumer: the vst-audio binding driven from Tk's event loop."""
import queue
import sys
from typing import Callable

import customtkinter as ctk

import common

TOOLKIT = "CustomTkinter"
# How often the Tk thread runs callables other threads queued. It is separate
# from the heartbeat so a reply does not wait for the next heartbeat.
POLL_MS = 10


class Window:
    """The main window and the view the controller draws through."""

    def __init__(self, options: common.Options) -> None:
        self.code = 0
        # Tk is not thread-safe: other threads only queue callables, and a
        # short after() poll runs them on the Tk thread.
        self._posted: "queue.SimpleQueue[Callable[[], None]]" = queue.SimpleQueue()
        self.controller = common.Controller(TOOLKIT, options, self, self._posted.put)

        self.root = ctk.CTk()
        self.root.title(f"vst-audio · {TOOLKIT}")
        self.root.minsize(520, 240)
        self.root.protocol("WM_DELETE_WINDOW", self.root.quit)
        self._name = ctk.CTkLabel(self.root, anchor="w")
        self._status = ctk.CTkLabel(self.root, anchor="w")
        self._details = ctk.CTkLabel(self.root, anchor="w", justify="left", wraplength=560)
        for label in (self._name, self._status, self._details):
            label.pack(fill="x", padx=16, pady=(12, 0))
        row = ctk.CTkFrame(self.root, fg_color="transparent")
        row.pack(fill="x", padx=16, pady=16)
        self._buttons = []
        for text, action in (("Open editor", self.controller.open_editor),
                             ("Close editor", self.controller.close_editor),
                             ("Render test tone", self.controller.render)):
            button = ctk.CTkButton(row, text=text, command=action, state="disabled")
            button.pack(side="left", padx=(0, 8))
            self._buttons.append(button)
        self._poll_id = self.root.after(POLL_MS, self._poll)
        self._beat_id = self.root.after(common.HEARTBEAT_MS, self._beat)

    def refresh(self, state: common.State) -> None:
        self._name.configure(text=state.plugin)
        self._status.configure(text=state.status)
        self._details.configure(text=state.details)
        for button in self._buttons:
            button.configure(state="normal" if state.ready else "disabled")

    def finish(self, code: int) -> None:
        self.code = code
        self.root.quit()

    def run(self) -> int:
        self.controller.start()
        try:
            self.root.mainloop()
        finally:
            self.root.after_cancel(self._poll_id)
            self.root.after_cancel(self._beat_id)
            self.controller.shutdown()
            self.root.destroy()
        return self.code

    def _poll(self) -> None:
        while True:
            try:
                function = self._posted.get_nowait()
            except queue.Empty:
                break
            function()
        self._poll_id = self.root.after(POLL_MS, self._poll)

    def _beat(self) -> None:
        self.controller.heartbeat()
        self._beat_id = self.root.after(common.HEARTBEAT_MS, self._beat)


def main() -> None:
    options = common.parse_args(TOOLKIT)
    sys.exit(Window(options).run())


if __name__ == "__main__":
    main()
