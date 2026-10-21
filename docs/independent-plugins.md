# Independent plugin compatibility tests

`python tools/third_party.py` builds and tests unmodified upstream DSP code on
the current native platform. CI requires Windows x64, Linux x64, macOS ARM64
and macOS Intel. Sources and native builds live outside the checkout in a
temporary directory and are removed after the run. Only `summary.json` and a
bounded `checks.log` are uploaded; plugin binaries, source trees and memory
dumps are not artifacts or cache entries.

Pinned sources:

- [DPF 4238e1c](https://github.com/DISTRHO/DPF/tree/4238e1c7f0351bbe488d79f0899c540543ac7583):
  MIDI Through and Latency, both VST2 and VST3. The harness only selects the
  existing DSP source files and build targets; it does not patch upstream code.
- [VST3 SDK 3cdf9c](https://github.com/steinbergmedia/vst3sdk/tree/3cdf9ca5d1f5b1b21e0a86832aa4abe55607bd96):
  ADelay and mda DX10. SDK submodule commits are resolved from this pinned tree.

| Plugin | Required semantic assertion |
|---|---|
| DPF MIDI Through | All 16 MIDI channels; 96 note, pressure, CC and pitch-bend events retain type, values, bus and sample offset |
| DPF MIDI Through | VST2 program change round trip; VST3 reports a rejected program change because this plugin has no program parameter |
| DPF Latency | Zero-latency setting produces sample-for-sample ramp output in both formats |
| DPF Latency | Stereo and f64 configuration requests are rejected; the independently running mono instance stays healthy |
| DPF Latency | Hidden read-only latency meter is distinguishable from its editable control and refuses writes |
| ADelay | Accepted mono, stereo and 5.1 layouts are read back exactly; every channel has the upstream minimum one-sample delay |
| ADelay | Mismatched input/output arrangements are rejected |
| mda DX10 | Program change plus note on/off reaches the real synth; the prefix before the note offset is silent and subsequent audio is finite and nonzero |
| mda DX10 | Two fresh instances with identical events produce matching audio |

Measurements record expected and observed values. A successful load or a
non-crashing process is never sufficient to pass these tests. The suite proves
these specific combinations; each additional plugin or capability needs its
own semantic assertions. Public fixtures separately cover f64, sidechains,
auxiliary buses, sample-accurate automation, transport, failure and recovery.

For incremental local development, `--root <outside-checkout-directory>` keeps
the explicitly selected build directory. CI uses an automatically cleaned
temporary directory. Building does not install plugins into the user's VST
directories, and no registry or package publication occurs.
