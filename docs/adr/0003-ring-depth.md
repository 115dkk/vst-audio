# Ring depth and long callbacks (2026-10-23)

Accepted. Layout version 3 introduced configurable contiguous sample slots
(`Geometry::slots`, header word 14) instead of exactly two. Layout version 4
restricts them to powers of two: 2, 4, 8 or 16; see
[ADR 0005](0005-serial-sequences.md). A publish reuses the slot of
`sequence.wrapping_sub(slots)` only after the worker completed it, and a read
refuses a slot that a later sequence already overwrote. `Pipeline` reads the block
`depth` blocks back instead of always the previous one; publish-then-read needs
`depth <= slots - 1`, read-then-publish allows `depth <= slots`.

Why: with two slots and depth 1, a caller that crosses several quantum
boundaries in one call loses blocks even when the worker is fast. Measured on
the native fixtures with real-time-paced callbacks over the default 128-frame
quantum, 256-frame callbacks fell back for half of the calls and 480- or
512-frame callbacks for every call: the second boundary of a call looked for a
block published microseconds earlier, and the third could not publish at all.
An application that places 1536-frame decoded scenes into 480-frame blocks
never received a plugin block.

`AudioEndpoint` takes `Configuration::max_callback_frames` (default: the
quantum). It uses `depth = ceil(max_callback_frames / quantum)` and the next
power of two at or above `depth + 1` slots, reports `(depth + 1) * quantum` plus
plugin latency, and refuses longer callbacks with `InvalidBuffer` and zeroed
outputs before touching its state.
The default stays depth 1, two slots and two quanta. Low-level users choose
`ProcessingOptions::slots` and `Pipeline::with_depth` themselves.

The follow-up [block endpoint decision](0004-block-endpoint.md) centralizes this
client logic in `BlockLane` and exposes it through `BlockEndpoint`.
