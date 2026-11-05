"""PySide6 consumer: the vst-audio binding driven from Qt's event loop."""
import sys

from PySide6 import QtCore, QtWidgets

import qt_view

if __name__ == "__main__":
    sys.exit(qt_view.main("PySide6", QtCore, QtWidgets, QtCore.Signal))
