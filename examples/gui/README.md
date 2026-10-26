# Independent GUI consumers

These examples have a separate Cargo workspace so GUI dependencies never enter
the library's normal build. egui, iced, Slint and Tauri consume the same public
request/event API. The native plugin editor remains in its own worker process.

```text
cargo build -p vst-audio-worker -p vst-audio-testplugin
cargo run --manifest-path examples/gui/Cargo.toml -p vst-audio-egui-example -- --worker <absolute worker path> --plugin <absolute plugin path>
cargo run --manifest-path examples/gui/Cargo.toml -p vst-audio-iced-example -- --worker <absolute worker path> --plugin <absolute plugin path>
cargo run --manifest-path examples/gui/Cargo.toml -p vst-audio-slint-example -- --worker <absolute worker path> --plugin <absolute plugin path>
cargo run --manifest-path examples/gui/Cargo.toml -p vst-audio-tauri-example -- --worker <absolute worker path> --plugin <absolute plugin path>
```

`--smoke` automatically loads, opens, waits for the editor response, closes it,
waits for EditorClosed and exits. Missing responses or native-window setup
failures cause a nonzero exit. It does not require a physical audio device.

The Tauri example serves `tauri-host/ui` without a JavaScript build step. Its
page polls the model through a Tauri command, so its smoke run also needs the
webview, the page script and the IPC bridge to work. On Linux it needs
WebKitGTK 4.1 (`libwebkit2gtk-4.1-dev` on Debian and Ubuntu); Windows needs
the WebView2 runtime.
