#![allow(unsafe_code)]

//! Default-off helper-process fixture for worker process-group teardown.

use std::process::{Command, Stdio};

// Same constructor layout as ctor 1.0.13's documented expansion. This library
// is dlopened by an already-running worker, not linked into the application.
// SAFETY: the platform loader expects a static C-ABI, no-argument function
// pointer in this initialization section; spawn_helper has exactly that ABI.
#[used]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
#[cfg_attr(
    target_os = "macos",
    unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")
)]
static INITIALIZE: extern "C" fn() = spawn_helper;

extern "C" fn spawn_helper() {
    let Some(path) = std::env::var_os("VST_AUDIO_TEST_SPAWN_HELPER") else {
        return;
    };
    // No process_group override: the helper must inherit the worker's group.
    // Null stdio and exec's CLOEXEC handling avoid keeping worker IPC alive.
    // A finite lifetime also bounds cleanup if the entire test runner dies.
    let Ok(mut helper) = Command::new("/bin/sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if std::fs::write(path, format!("{} {}\n", std::process::id(), helper.id())).is_err() {
        let _ = helper.kill();
        let _ = helper.wait();
    }
    // Intentionally leave the helper alive when the plugin/worker exits.
}
