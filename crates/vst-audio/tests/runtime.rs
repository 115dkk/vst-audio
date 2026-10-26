#![forbid(unsafe_code)]

#[cfg(unix)]
use std::{
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
const HELPER_PID_FILE: &str = "VST_AUDIO_PARENT_DEATH_PID_FILE";

#[cfg(unix)]
fn paths() -> (PathBuf, PathBuf) {
    (
        std::env::var_os("VST_AUDIO_WORKER")
            .expect("set VST_AUDIO_WORKER to the worker executable")
            .into(),
        std::env::var_os("VST_AUDIO_TEST_VST2")
            .expect("set VST_AUDIO_TEST_VST2 to the test plugin")
            .into(),
    )
}

#[cfg(unix)]
#[test]
#[ignore = "needs vst-audio-worker and the test plugin; run by tools/native_tests.py"]
fn worker_exits_after_parent_process_dies() {
    if let Some(pid_file) = std::env::var_os(HELPER_PID_FILE) {
        let (worker, plugin) = paths();
        let started = vst_audio::PluginProcess::start(
            &worker,
            &plugin,
            vst_audio::PluginFormat::Vst2,
            None,
            48_000,
            Some(&[1, 2]),
        )
        .expect("helper parent starts worker");
        assert_eq!(
            started.publisher.try_publish(
                started.region.region(),
                0,
                1,
                Some(&*started.work),
                |slot| {
                    for frame in 0..slot.frames() {
                        slot.write(0, frame, 0.25);
                    }
                },
            ),
            vst_audio_ring::Publish::Published
        );
        std::fs::write(pid_file, started.control.pid().to_string()).expect("write worker PID");
        std::process::exit(0);
    }

    let directory = tempfile::tempdir().expect("create PID directory");
    let pid_file = directory.path().join("worker.pid");
    let status = Command::new(std::env::current_exe().expect("locate test executable"))
        .args([
            "--exact",
            "worker_exits_after_parent_process_dies",
            "--ignored",
            "--nocapture",
        ])
        .env(HELPER_PID_FILE, &pid_file)
        .status()
        .expect("run helper parent");
    assert!(status.success(), "helper parent failed: {status}");

    let pid = std::fs::read_to_string(&pid_file)
        .expect("read worker PID")
        .trim()
        .parse::<u32>()
        .expect("parse worker PID");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success());
        if !alive {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "worker {pid} remained alive after its parent exited"
        );
        thread::sleep(Duration::from_millis(25));
    }
}
