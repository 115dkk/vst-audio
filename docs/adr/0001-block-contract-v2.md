# Block contract v2 (2026-10-21)

Accepted. Each ring slot carries planar f32 or f64 audio, a transport snapshot,
and bounded input/output event packets. All shared bytes remain AtomicU32;
f64 uses two words under the existing publish/complete ownership protocol.
Pointers and borrowed plugin memory never cross IPC. The layout version changes
before release; old workers are rejected by the handshake/header.

Events preserve sample offsets, bus indexes and format-specific parameter IDs.
MIDI channel messages, bounded SysEx, parameter automation and transport changes
are represented explicitly. Overflow and unsupported events are reported.
VST2 automation and in-block transport changes split processing at boundaries;
VST3 parameter queues retain sample offsets. Buses are configured before audio
activation, retain native order, and may have discrete/unknown speaker layouts.

The application reblocks variable callbacks into a prepared quantum. Its fixed
pipeline delay is two quanta, plus reported plugin latency (configurable since
[ADR 0003](0003-ring-depth.md)). Recovery transfers
prebuilt resources through bounded SPSC queues; retired resources are reclaimed
on the supervisor thread, never in the audio callback.
