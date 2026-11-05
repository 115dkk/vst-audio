"""Qt Widgets window shared by the PyQt6 and PySide6 consumers."""
import sys
from types import ModuleType
from typing import Any, Callable

import common


def main(toolkit: str, QtCore: ModuleType, QtWidgets: ModuleType, Signal: Any) -> int:
    """Run the window with one binding's modules and its signal class; return the exit code."""
    options = common.parse_args(toolkit)
    # Qt's own options stay out of our argument list and ours out of Qt's.
    app = QtWidgets.QApplication(sys.argv[:1])

    class Poster(QtCore.QObject):
        """Runs callables on the GUI thread. ``posted.emit`` is safe from any thread."""

        posted = Signal(object)

        def __init__(self) -> None:
            super().__init__()
            # The receiver lives on the GUI thread, so a queued call runs there.
            self.posted.connect(self._run, QtCore.Qt.ConnectionType.QueuedConnection)

        def _run(self, function: Callable[[], None]) -> None:
            function()

    class Window:
        """The main window and the view the controller draws through."""

        def __init__(self) -> None:
            self._poster = Poster()
            self.controller = common.Controller(toolkit, options, self, self._poster.posted.emit)
            self.widget = QtWidgets.QWidget()
            self.widget.setWindowTitle(f"vst-audio · {toolkit}")
            self.widget.resize(520, 200)
            self._name = QtWidgets.QLabel()
            self._status = QtWidgets.QLabel()
            # No word wrap: the window grows to the layout's minimum size, which
            # does not follow a wrapped label's height.
            self._details = QtWidgets.QLabel()
            layout = QtWidgets.QVBoxLayout(self.widget)
            layout.setContentsMargins(16, 16, 16, 16)
            layout.setSpacing(12)
            for label in (self._name, self._status, self._details):
                layout.addWidget(label)
            row = QtWidgets.QHBoxLayout()
            self._buttons = []
            for text, action in (("Open editor", self.controller.open_editor),
                                 ("Close editor", self.controller.close_editor),
                                 ("Render test tone", self.controller.render)):
                button = QtWidgets.QPushButton(text)
                button.setEnabled(False)
                button.clicked.connect(action)
                row.addWidget(button)
                self._buttons.append(button)
            row.addStretch()
            layout.addLayout(row)
            layout.addStretch()
            self._heartbeat = QtCore.QTimer(self.widget)
            self._heartbeat.timeout.connect(self.controller.heartbeat)
            self._heartbeat.start(common.HEARTBEAT_MS)

        def refresh(self, state: common.State) -> None:
            self._name.setText(state.plugin)
            self._status.setText(state.status)
            self._details.setText(state.details)
            for button in self._buttons:
                button.setEnabled(state.ready)

        def finish(self, code: int) -> None:
            QtWidgets.QApplication.exit(code)

    window = Window()
    window.widget.show()
    window.controller.start()
    code = app.exec()
    window.controller.shutdown()
    return code
