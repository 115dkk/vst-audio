# Modulo-2^32 serial sequences (2026-10-24)

Accepted. Ring layout version 4 treats every lane sequence and completion as a
modulo-2^32 serial number. Zero is an ordinary value. The producer and consumer
advance with wrapping addition, and serial comparisons use signed wrapping
distance. The maintained invariant is
`0 <= D(published, completed) <= slots <= 16 < 2^31`, so comparisons are
unambiguous. Every lane has 2, 4, 8 or 16 slots, and slot selection is
`sequence & (slots - 1)`.

A producer may announce a configurable initial sequence for deterministic tests;
normal application startup uses zero. Announcement writes that origin to both
the published and completed counters. Publication always requires completion of
`sequence.wrapping_sub(slots)`, including the first publications after an
arbitrary origin. This removes the previous counter-exhaustion state and permits
continuous operation across zero without allocation, locking or waiting.

Alternatives rejected:

- `u64` counters only move the bound far away. They enlarge and realign the
  shared header, give the i686 worker a new 8-byte atomic access width, and
  still need wrapping arithmetic to be correct.
- Per-slot generation markers add atomics and validation to the realtime path.
- Counting modulo the largest multiple of the slot count below 2^32 keeps
  arbitrary slot counts, but every successor, distance and comparison then
  depends on the geometry, the signed 32-bit comparison no longer works, and
  `Pipeline`'s 16-entry index overlaps at the wrap.
- Reannouncing at `u32::MAX` disrupts a live worker and turns a local counter
  boundary into lifecycle coordination.

Layout version 4 is intentionally incompatible with version 3. Header validation
rejects old layouts and non-power-of-two slot counts. Control request IDs use the
same operational principle but retain protocol-reserved values: after
`u32::MAX`, application requests wrap to 6 and never use 0 through 5. Responses
continue to match requests by equality; pending frames and the watch retain the
exact wrapped ID.
