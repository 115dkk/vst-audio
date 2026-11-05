"""PyQt6 consumer: the vst-audio binding driven from Qt's event loop."""
import sys

from PyQt6 import QtCore, QtWidgets

import qt_view

if __name__ == "__main__":
    sys.exit(qt_view.main("PyQt6", QtCore, QtWidgets, QtCore.pyqtSignal))
