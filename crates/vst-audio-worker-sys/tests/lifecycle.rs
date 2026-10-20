#![forbid(unsafe_code)]
use std::path::Path;
use vst_audio_worker_sys::plugin::Vst2Plugin;

#[test]
#[ignore = "requires the public native fixture built by tools/native_tests.py"]
fn processor_is_unique_and_freezes_configuration() {
    let path = std::env::var_os("VST_AUDIO_TEST_VST2").expect("fixture required");
    let mut plugin = Vst2Plugin::load(Path::new(&path)).unwrap();
    assert!(plugin.processor().is_err());
    plugin.configure(48000, 1024, 2, 6).unwrap();
    let processor = plugin.processor().unwrap();
    assert!(plugin.processor().is_err());
    // An old processor must never send 1024 frames after a 64-frame reconfiguration.
    assert!(plugin.configure(48000, 64, 2, 6).is_err());
    drop(processor);
}
