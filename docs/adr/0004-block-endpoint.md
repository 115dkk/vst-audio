# Block endpoints and the ring lane client (2026-10-23)

Accepted. Block-synchronous processing has three layers. `vst_audio_ring::BlockLane`
is the portable ring client and owns publishing, fixed-depth output selection and
sequence accounting for one lane. Applications that may link only
`vst-audio-ring` use it directly. `vst_audio::BlockEndpoint` adds a high-level
instance's status and recovery generation handling. `AudioEndpoint` is the
variable-callback adapter on top, adding reblocking, events, transport and
silence or delayed-input fallback.

The block contract submits block `k` and returns block `k - depth`. A result is
returned only when that exact block completed in time; a miss leaves output
untouched so the caller can provide its own fallback. Recovery is adopted only
at the start of a block call, then the replacement lane restarts at block 0 with
the endpoint's current depth.

Why: until this change `AudioEndpoint` and an application that drives the ring
itself (Spatial SoftReceiver's upmix stage) each kept their own copy of
publishing, pipeline bookkeeping and sequence numbers, so every ring fix had to be
made twice. A block caller also had no high-level handle: it had to drive the
low-level `PluginProcess` and rebuild its audio side after every recovery.

`Configuration::slots` chooses the number of ring slots per lane. Its default is
callback depth plus one; an explicit value must be at least that large and at
most `MAX_SLOTS`. A `BlockEndpoint` can restart at any depth from 1 through
`slots - 1`.

Consequences: `BlockEndpoint` reports `depth * quantum + plugin latency`.
`AudioEndpoint` retains its extra reblocking quantum and reports
`(depth + 1) * quantum + plugin latency`. Miss concealment belongs only to the
adapter or the block caller, while ring publication and sequence rules have one
implementation.

The adapter now works in plane-major segments between quantum boundaries: it
reads the running status once per segment, copies whole planes into and out of
the slot, clears only the slot planes beyond the inputs, and moves the
delayed-input history with slice copies (reading the delayed segment before
writing the new one). An in-process golden test of 96 scenarios (f32 and f64,
both fallbacks, quantum 128 and 480, callbacks up to 480 frames, events,
transport changes, late and failed blocks, plugin latency at and below the
history bound) gives the same hashes as the 0.3.0 adapter. Measured with the
same in-process consumer (release build, f32, 2 inputs, 6 outputs, 480-frame
callbacks), the median cost per callback fell from 12.8 to 3.8 µs (silence) and
from 18.1 to 4.4 µs (delayed input) at quantum 480, and from 13.8 to 5.4 µs and
from 19.7 to 6.0 µs at quantum 128.
