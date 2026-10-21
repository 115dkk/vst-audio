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
