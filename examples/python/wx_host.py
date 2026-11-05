"""wxPython consumer: the vst-audio binding driven from wxWidgets' event loop."""
import sys

import wx

import common

TOOLKIT = "wxPython"


class Window:
    """The main frame and the view the controller draws through."""

    def __init__(self, options: common.Options) -> None:
        self.code = 0
        self._closed = False
        self.controller = common.Controller(TOOLKIT, options, self, wx.CallAfter)
        self.frame = wx.Frame(None, title=f"vst-audio · {TOOLKIT}")
        panel = wx.Panel(self.frame)
        self._name = wx.StaticText(panel)
        self._status = wx.StaticText(panel)
        self._details = wx.StaticText(panel)
        sizer = wx.BoxSizer(wx.VERTICAL)
        for label in (self._name, self._status, self._details):
            sizer.Add(label, 0, wx.EXPAND | wx.LEFT | wx.RIGHT | wx.TOP, 16)
        row = wx.BoxSizer(wx.HORIZONTAL)
        self._buttons = []
        for text, action in (("Open editor", self.controller.open_editor),
                             ("Close editor", self.controller.close_editor),
                             ("Render test tone", self.controller.render)):
            button = wx.Button(panel, label=text)
            button.Disable()
            button.Bind(wx.EVT_BUTTON, lambda _event, action=action: action())
            row.Add(button, 0, wx.RIGHT, 8)
            self._buttons.append(button)
        sizer.Add(row, 0, wx.ALL, 16)
        panel.SetSizer(sizer)
        self.frame.SetMinClientSize((520, 220))
        self.frame.Fit()
        self.frame.Bind(wx.EVT_CLOSE, self._on_close)
        self._heartbeat = wx.Timer(self.frame)
        self.frame.Bind(wx.EVT_TIMER, lambda _event: self.controller.heartbeat(), self._heartbeat)
        self._heartbeat.Start(common.HEARTBEAT_MS)

    def refresh(self, state: common.State) -> None:
        if self._closed:  # a call queued before the frame closed
            return
        self._name.SetLabel(state.plugin)
        self._status.SetLabel(state.status)
        self._details.SetLabel(state.details)
        for button in self._buttons:
            button.Enable(state.ready)
        self.frame.Layout()

    def finish(self, code: int) -> None:
        self.code = code
        self.frame.Close()

    def _on_close(self, event: wx.CloseEvent) -> None:
        self._closed = True
        self._heartbeat.Stop()
        event.Skip()  # the default handler destroys the frame; the loop ends with it


def main() -> None:
    options = common.parse_args(TOOLKIT)
    app = wx.App()
    window = Window(options)
    window.frame.Show()
    window.controller.start()
    app.MainLoop()
    window.controller.shutdown()
    sys.exit(window.code)


if __name__ == "__main__":
    main()
