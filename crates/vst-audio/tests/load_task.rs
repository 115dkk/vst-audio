#![forbid(unsafe_code)]

#[cfg(any(windows, target_os = "macos"))]
use std::process::Command;
#[cfg(target_os = "macos")]
use std::process::Stdio;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{AudioStatus, Configuration, Host, PluginDescriptor, PluginFormat, Status};

fn descriptor(format: PluginFormat) -> PluginDescriptor {
    PluginDescriptor::new(
        PathBuf::from(
            std::env::var_os(if format == PluginFormat::Vst2 {
                "VST_AUDIO_TEST_VST2"
            } else {
                "VST_AUDIO_TEST_VST3"
            })
            .expect("fixture required"),
        ),
        format,
    )
}

fn host() -> Host {
    Host::new(PathBuf::from(
        std::env::var_os("VST_AUDIO_WORKER").expect("worker required"),
    ))
}

fn state(format: PluginFormat, mode: u8) -> Vec<u8> {
    if format == PluginFormat::Vst3 {
        vst_audio::state::encode(&[1, mode], &[1, mode]).unwrap()
    } else {
        vec![1, mode]
    }
}

fn process_exists(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("probe process existence")
            .success()
    }
    #[cfg(windows)]
    {
        let output = Command::new("tasklist.exe")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .creation_flags(0x08000000)
            .output()
            .expect("probe process existence");
        output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.trim_start().starts_with('"'))
    }
}

fn worker_pids() -> Vec<u32> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let worker =
        std::fs::canonicalize(std::env::var_os("VST_AUDIO_WORKER").expect("worker required"))
            .expect("canonical worker path");
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir("/proc")
            .expect("read /proc")
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| {
                std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|path| path == worker)
            })
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("ps")
            .args(["-axo", "pid=,command="])
            .output()
            .expect("list processes");
        let worker = worker.to_string_lossy();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.trim().splitn(2, char::is_whitespace);
                let pid = fields.next()?.parse().ok()?;
                fields
                    .next()?
                    .trim_start()
                    .starts_with(worker.as_ref())
                    .then_some(pid)
            })
            .collect()
    }
    #[cfg(windows)]
    {
        let output = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$target=[IO.Path]::GetFullPath($env:VST_AUDIO_WORKER); Get-CimInstance Win32_Process | Where-Object {$_.ExecutablePath -and [IO.Path]::GetFullPath($_.ExecutablePath) -eq $target} | ForEach-Object {$_.ProcessId}",
            ])
            .creation_flags(0x08000000)
            .output()
            .expect("list worker processes");
        assert!(output.status.success(), "PowerShell process query failed");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect()
    }
}

#[cfg(windows)]
trait CommandNoWindow {
    fn creation_flags(&mut self, flags: u32) -> &mut Self;
}

#[cfg(windows)]
impl CommandNoWindow for Command {
    fn creation_flags(&mut self, flags: u32) -> &mut Self {
        use std::os::windows::process::CommandExt;
        CommandExt::creation_flags(self, flags)
    }
}

fn wait_for_exit(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_exists(pid) {
        assert!(
            Instant::now() < deadline,
            "worker PID {pid} survived cancellation"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_instance(task: &vst_audio::LoadTask) -> vst_audio::Instance {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(instance) = task.try_complete().unwrap() {
            return instance;
        }
        assert!(Instant::now() < deadline, "load task did not complete");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_new_worker(before: &[u32]) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pid) = worker_pids().into_iter().find(|pid| !before.contains(pid)) {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "load task did not start a worker"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "native fixtures required"]
fn dropping_before_completion_cancels_start() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let before = worker_pids();
        let task = host().spawn_async(
            descriptor(format),
            Configuration::default(),
            Some(state(format, 5)),
        );
        let pid = wait_for_new_worker(&before);
        drop(task);
        wait_for_exit(pid);
    }
}

#[test]
#[ignore = "native fixtures required"]
fn dropping_an_undelivered_result_leaves_no_worker() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let before = worker_pids();
        let task = host().spawn_async(descriptor(format), Configuration::default(), None);
        let pid = wait_for_new_worker(&before);
        std::thread::sleep(Duration::from_millis(100));
        assert!(process_exists(pid), "worker exited before LoadTask drop");
        drop(task);
        wait_for_exit(pid);
    }
}

#[test]
#[ignore = "native fixtures required"]
fn dropping_after_delivery_keeps_instance_running() {
    for format in [PluginFormat::Vst2, PluginFormat::Vst3] {
        let task = host().spawn_async(descriptor(format), Configuration::default(), None);
        let mut instance = wait_for_instance(&task);
        drop(task);
        assert_eq!(instance.status(), Status::Running);

        let mut audio = instance.take_audio().unwrap();
        let left = [0.25f32; 128];
        let right = [0.125f32; 128];
        let mut output = [[0.0f32; 128]; 6];
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut refs: Vec<_> = output.iter_mut().map(|plane| &mut plane[..]).collect();
            if audio.process(&[&left, &right], &mut refs) == AudioStatus::Processed
                && (output[0][100] - 0.25).abs() < 1e-6
                && (output[5][100] - 0.0625).abs() < 1e-6
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "delivered instance stopped processing"
            );
            std::thread::sleep(Duration::from_millis(3));
        }
    }
}
