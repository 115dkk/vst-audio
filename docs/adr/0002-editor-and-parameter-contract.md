# Editor and parameter contract, protocol 3 (2026-10-21)

Accepted. Native editor operations remain main-thread worker requests, with
explicit status or error replies. Hide preserves the plugin view; close removes
it. Focus requests follow OS policy. VST3 resize uses canResize,
checkSizeConstraint and onSize; content scaling requires the optional
IPlugViewContentScaleSupport interface. VST2 has no standard corresponding
host-driven resize/scale operation and returns an explicit error.

Editor capture returns bounded owned RGBA bytes, never a native pointer. It is
an opt-in control operation; captures do not run on the realtime thread. Native
view capture is useful for diagnostics but does not guarantee GPU-rendered
third-party surfaces can be captured on every platform.

Parameter metadata now preserves automatable, read-only and hidden attributes.
This is a wire-breaking change and increments the protocol major to 3. Real DPF
Latency has two identically named parameters, one a hidden read-only meter;
name matching alone cannot identify an editable control. Read-only edits are
rejected before calling the plugin. VST3 program changes resolve program
parameters through unit/bus association, not IMidiMapping CC 130, which is a
legacy MIDI-output constant. Child interfaces are released before IComponent.

## Opening scale policy (2026-10-23)

`EditorScaling` adds `Auto`, `Plugin`, `Stretch` and `Off`. Existing application
open requests select `Auto`; an explicit request includes an optional parent
and mode. Status reports the resolved mode. Open messages (kinds 7 and 30) and
editor status append a mode byte, with absent bytes decoded as `Auto` and
unknown values rejected. The protocol minor version increases; the major and
unknown-kind handling are unchanged.

`Auto` prefers plugin scaling, falls back to system stretching for Windows
floating editors, and otherwise leaves the editor 1:1. `Plugin` never uses
system stretching. `Stretch` needs a floating window on Windows, or a Wayland
session with the desktop's XWayland, where the editor opens as an X11 window at
scale 1 for the compositor to scale; elsewhere it is a request error. `Off` sends
no plugin scale request. macOS retains its existing Retina behavior for all modes.

VST2 has no standard scale opcode, but plugins advertising
`supportsViewDpiScaling` accept the de-facto vendor request: opcode 50,
index `PreS` (`0x50726553`), value `AeCs` (`0x41654373`), null pointer and scale
in `opt`. Send it before the first `effEditGetRect`, and reread the rectangle
after later scale requests. Host-driven VST2 resize remains unsupported.
VST3 uses its optional content-scale interface before attachment and only
reports plugin scaling when the request succeeds.

Windows editor creation uses a restoring thread DPI scope: per-monitor-v2 for
plugin/unscaled windows, unaware-GDI-scaled for stretched windows. Differently
aware embedded parents also require a temporary mixed-hosting scope, without
changing the application's parent policy. Window/parent DPI supplies initial
scale; `WM_DPICHANGED` renegotiates plugin size or applies the suggested rectangle.
X11 reads `Xft.dpi` from root `RESOURCE_MANAGER`, divides by 96 and clamps the
finite positive result to 0.5–4.0; invalid or missing resources use 1.0.
Native Wayland keeps its existing integer output-scale convention, except that
`Off` suppresses content-scale requests. Other compositors and real mixed-DPI
multi-monitor setups still need native verification.

Windows DPI functions are looked up at run time, so a worker on a Windows
release without them still starts and opens editors at scale 1.

A private rootful Xwayland per editor is not implemented. Plugins open their
own X connection from `DISPLAY`, which would have to name the private server
before the plugin loads, and an X server cannot be stopped under a plugin's
connection because Xlib's default I/O error handler exits the process. VST 2 and
X11-only VST 3 editors in a Wayland session use the desktop's XWayland.
