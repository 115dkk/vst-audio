# Independent GUI consumers

These examples have a separate Cargo workspace so GUI dependencies never enter
the library's normal build. Both consume the same public request/event API.
The native plugin editor remains in its own worker process.

```text
cargo build -p vst-audio-worker -p vst-audio-testplugin
cargo run --manifest-path examples/gui/Cargo.toml -p vst-audio-egui-example -- --worker <absolute worker path> --plugin <absolute plugin path>
cargo run --manifest-path examples/gui/Cargo.toml -p vst-audio-iced-example -- --worker <absolute worker path> --plugin <absolute plugin path>
```

`--smoke` automatically loads, opens, waits for the editor response, closes it,
waits for EditorClosed and exits. Missing responses or native-window setup
failures cause a nonzero exit. It does not require a physical audio device.
