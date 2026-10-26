#![forbid(unsafe_code)]
#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use vst_audio::{Configuration, Host, PluginDescriptor, PluginFormat, RecoveryPolicy};

const TEST: &str = "drop_removes_helper_after_worker_leader_exits";

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn exists(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        // Zombies still have a PID. Do not count a killed but unreaped helper
        // as successful teardown.
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("probe process existence")
            .success()
    }
}

fn kill(pid: u32) {
    let _ = Command::new("/bin/kill")
        .args(["-KILL", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn pids(path: &Path) -> Option<(u32, u32)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut fields = text.split_whitespace();
    let leader = fields.next()?.parse().ok()?;
    let helper = fields.next()?.parse().ok()?;
    (leader > 1 && helper > 1 && leader != helper).then_some((leader, helper))
}

fn group(pid: u32) -> u32 {
    let output = Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .expect("read process group");
    assert!(output.status.success(), "ps failed for PID {pid}");
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .expect("numeric process group")
}

fn exercise(path: &Path) {
    let worker = PathBuf::from(std::env::var_os("VST_AUDIO_WORKER").expect("worker required"));
    let plugin = PathBuf::from(std::env::var_os("VST_AUDIO_TEST_VST2").expect("fixture required"));
    let config = Configuration {
        recovery: RecoveryPolicy::Manual,
        ..Configuration::default()
    };
    let instance = Host::new(worker)
        .spawn(
            &PluginDescriptor::new(plugin, PluginFormat::Vst2),
            &config,
            None,
        )
        .expect("load fixture through the isolated worker");
    assert!(
        wait_until(Duration::from_secs(3), || pids(path).is_some()),
        "fixture did not publish worker/helper PIDs"
    );
    let (leader, helper) = pids(path).unwrap();
    assert!(exists(leader), "worker was not alive after load");
    assert!(exists(helper), "helper was not alive after load");
    assert_eq!(group(leader), leader, "worker must lead its own group");
    assert_eq!(group(helper), leader, "helper must inherit worker group");
    kill(leader);
    assert!(
        wait_until(Duration::from_secs(5), || !exists(leader)),
        "worker leader PID {leader} was not reaped before Instance drop"
    );
    eprintln!("worker leader PID {leader} exited and was reaped before Instance drop");
    drop(instance);
    assert!(
        wait_until(Duration::from_secs(3), || !exists(helper)),
        "helper PID {helper} survived Instance drop after worker leader {leader} exited"
    );
}

#[test]
#[ignore = "native fixtures required"]
fn drop_removes_helper_after_worker_leader_exits() {
    if let Some(path) = std::env::var_os("VST_AUDIO_TEST_SPAWN_HELPER") {
        exercise(Path::new(&path));
        return;
    }
    // Only this subprocess and its workers receive the default-off fixture
    // switch. Never mutate the multithreaded test runner's environment.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("helper.pid");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            TEST,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("VST_AUDIO_TEST_SPAWN_HELPER", &path)
        .spawn()
        .expect("start isolated process-group regression");
    let mut status = None;
    let completed = wait_until(Duration::from_secs(20), || {
        status = child.try_wait().expect("wait for regression subprocess");
        status.is_some()
    });
    // Cleanup also runs when the inner assertion fails or times out. Keep its
    // outcome separate from the behavioral assertion, and require actual PID
    // disappearance rather than accepting an orphaned zombie.
    if !completed {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    if let Some((leader, helper)) = pids(&path) {
        for pid in [leader, helper] {
            if exists(pid) {
                kill(pid);
            }
            assert!(
                wait_until(Duration::from_secs(5), || !exists(pid)),
                "cleanup could not reap PID {pid}; orphan reaping is required"
            );
        }
    }
    assert!(completed, "process-group regression subprocess timed out");
    assert!(
        status.unwrap().success(),
        "process-group regression subprocess failed"
    );
}

const INHERITANCE_TEST: &str = "plugin_helper_inherits_no_worker_descriptors";

#[cfg(target_os = "linux")]
fn open_descriptors(pid: u32) -> Vec<u32> {
    let mut descriptors: Vec<u32> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .expect("read helper descriptors")
        .map(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect();
    descriptors.sort_unstable();
    descriptors
}

/// A plugin's helper process must not receive the worker's control pipe, shared
/// mapping, events or parent handle (audit #11 TD-03).
#[cfg(target_os = "linux")]
#[test]
#[ignore = "native fixtures required"]
fn plugin_helper_inherits_no_worker_descriptors() {
    if let Some(path) = std::env::var_os("VST_AUDIO_TEST_SPAWN_HELPER") {
        let worker = PathBuf::from(std::env::var_os("VST_AUDIO_WORKER").expect("worker required"));
        let plugin =
            PathBuf::from(std::env::var_os("VST_AUDIO_TEST_VST2").expect("fixture required"));
        let instance = Host::new(worker)
            .spawn(
                &PluginDescriptor::new(plugin, PluginFormat::Vst2),
                &Configuration::default(),
                None,
            )
            .expect("load fixture through the isolated worker");
        let path = Path::new(&path);
        assert!(
            wait_until(Duration::from_secs(3), || pids(path).is_some()),
            "fixture did not publish worker/helper PIDs"
        );
        let (_, helper) = pids(path).unwrap();
        // The helper's stdio is null; anything else came from the worker.
        assert_eq!(
            open_descriptors(helper),
            [0, 1, 2],
            "helper inherited worker descriptors"
        );
        drop(instance);
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("helper.pid");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            INHERITANCE_TEST,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("VST_AUDIO_TEST_SPAWN_HELPER", &path)
        .status()
        .expect("run isolated inheritance regression");
    if let Some((leader, helper)) = pids(&path) {
        for pid in [leader, helper] {
            if exists(pid) {
                kill(pid);
            }
        }
    }
    assert!(status.success(), "inheritance regression subprocess failed");
}
