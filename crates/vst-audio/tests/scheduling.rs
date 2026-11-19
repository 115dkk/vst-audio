#![forbid(unsafe_code)]
//! Worker audio-thread scheduling, as the VST2 fixture observes it from inside
//! its own `process` call (fixture mode 7, observation slots 12..15).
use std::{
    fmt,
    path::PathBuf,
    time::{Duration, Instant},
};
use vst_audio::{
    ControlEvent, PluginFormat, PluginProcess, ProcessingOptions, SandboxPolicy, Started,
};
use vst_audio_protocol::Message;
use vst_audio_ring::{Publish, Wait};

const PROCESS_CALLS: usize = 6;
const SCHEDULING: usize = 12;
const SCHEDULING_DETAIL_A: usize = 13;
const SCHEDULING_DETAIL_B: usize = 14;
const SCHEDULING_DETAIL_C: usize = 15;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const NORMAL: u32 = 1;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const REALTIME: u32 = 2;
const SAMPLE_RATE: u32 = 48000;
const QUANTUM: u32 = 128;

#[cfg(target_os = "linux")]
const FIFO_PRIORITY: u32 = vst_audio_transport_sys::runtime::FIFO_PRIORITY as u32;
#[cfg(target_os = "linux")]
const SCHED_OTHER: u32 = 0;
#[cfg(target_os = "linux")]
const SCHED_FIFO_RESET_ON_FORK: u32 = 0x4000_0001;

fn paths() -> (PathBuf, PathBuf) {
    (
        std::env::var_os("VST_AUDIO_WORKER")
            .expect("VST_AUDIO_WORKER required; run tools/scheduling_tests.py")
            .into(),
        std::env::var_os("VST_AUDIO_TEST_VST2")
            .expect("VST_AUDIO_TEST_VST2 required; run tools/scheduling_tests.py")
            .into(),
    )
}
fn wait(s: &mut Started, mut accepts: impl FnMut(&Message) -> bool) -> Message {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        for event in s.control.poll(s.control.elapsed_ms()) {
            if let ControlEvent::Message(frame) = event
                && accepts(&frame.message)
            {
                return frame.message;
            }
        }
        assert!(Instant::now() < deadline, "message timeout");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn state(s: &mut Started) -> Vec<u8> {
    s.control.get_state().unwrap();
    let Message::State { blob } = wait(s, |m| matches!(m, Message::State { .. })) else {
        unreachable!()
    };
    blob
}
fn observation(blob: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(blob[2 + index * 4..6 + index * 4].try_into().unwrap())
}
fn block(s: &Started, label: &str, sequence: u32) {
    assert_eq!(
        s.publisher
            .try_publish(s.region.region(), 0, sequence, Some(&*s.work), |slot| {
                for f in 0..slot.frames() {
                    slot.write(0, f, 1.0);
                    slot.write(1, f, 1.0);
                }
            }),
        Publish::Published,
        "{label}: block {sequence} was not published"
    );
    let end = Instant::now() + Duration::from_secs(2);
    while s.publisher.completed(s.region.region(), 0) < sequence {
        assert!(
            Instant::now() < end,
            "{label}: worker did not complete block {sequence}"
        );
        s.done.wait(Duration::from_millis(2));
    }
}

fn options(offline: bool, sandbox: Option<SandboxPolicy>) -> ProcessingOptions {
    ProcessingOptions {
        sample_rate: SAMPLE_RATE,
        quantum: QUANTUM,
        offline,
        sandbox,
        ..Default::default()
    }
}

/// Raw fixture observations after the worker processed audio.
#[derive(Clone, Copy, Debug)]
struct Observed {
    process_calls: u32,
    scheduling: u32,
    a: u32,
    b: u32,
    c: u32,
}
impl fmt::Display for Observed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "process calls={}, slot12 scheduling={}, slot13={} ({:#010x}), slot14={}, slot15={}",
            self.process_calls, self.scheduling, self.a, self.a, self.b, self.c
        )
    }
}

/// Starts a worker on the mode-7 fixture, processes a few blocks and returns
/// what the fixture recorded about its `process` thread.
fn observe(label: &str, options: &ProcessingOptions) -> Observed {
    let (worker, plugin) = paths();
    let mut s = PluginProcess::start_configured(
        &worker,
        &plugin,
        PluginFormat::Vst2,
        None,
        options,
        Some(&[1, 7]),
    )
    .unwrap_or_else(|error| panic!("{label}: worker failed to start: {error:?}"));
    let sandbox = match (&options.sandbox, &s.sandbox) {
        (None, None) => "none".to_owned(),
        (Some(_), Some(report)) => {
            assert!(
                !report.mechanism.is_empty(),
                "{label}: empty sandbox mechanism"
            );
            report.mechanism.clone()
        }
        (requested, reported) => {
            panic!("{label}: sandbox requested={requested:?}, reported={reported:?}")
        }
    };
    for sequence in 1..=3 {
        block(&s, label, sequence);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let blob = loop {
        let blob = state(&mut s);
        assert_eq!(
            blob.len(),
            98,
            "{label}: fixture state is not a mode-7 observation chunk: {blob:?}"
        );
        if observation(&blob, PROCESS_CALLS) != 0 {
            break blob;
        }
        assert!(
            Instant::now() < deadline,
            "{label}: fixture PROCESS_CALLS stayed 0 after three completed blocks"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let observed = Observed {
        process_calls: observation(&blob, PROCESS_CALLS),
        scheduling: observation(&blob, SCHEDULING),
        a: observation(&blob, SCHEDULING_DETAIL_A),
        b: observation(&blob, SCHEDULING_DETAIL_B),
        c: observation(&blob, SCHEDULING_DETAIL_C),
    };
    println!(
        "{label}: offline={}, sandbox={sandbox}, {observed}{}",
        options.offline,
        platform_summary()
    );
    s.control
        .shutdown()
        .unwrap_or_else(|error| panic!("{label}: shutdown failed: {error:?}"));
    observed
}

#[cfg(target_os = "linux")]
fn rtprio_soft() -> Option<u32> {
    const LABEL: &str = "Max realtime priority";
    // The worker inherits this process's resource limits.
    let limits = std::fs::read_to_string("/proc/self/limits").expect("read /proc/self/limits");
    let line = limits
        .lines()
        .find(|line| line.starts_with(LABEL))
        .unwrap_or_else(|| panic!("no '{LABEL}' line in /proc/self/limits:\n{limits}"));
    let soft = line[LABEL.len()..]
        .split_whitespace()
        .next()
        .unwrap_or_else(|| panic!("no soft limit in {line:?}"));
    (soft != "unlimited").then(|| {
        soft.parse()
            .unwrap_or_else(|error| panic!("soft RLIMIT_RTPRIO {soft:?}: {error}"))
    })
}
#[cfg(target_os = "linux")]
fn has_cap_sys_nice() -> bool {
    const CAP_SYS_NICE: u32 = 23;
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    let hex = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .unwrap_or_else(|| panic!("no CapEff line in /proc/self/status"))
        .trim();
    let effective = u64::from_str_radix(hex, 16)
        .unwrap_or_else(|error| panic!("CapEff {hex:?} is not hexadecimal: {error}"));
    effective & (1 << CAP_SYS_NICE) != 0
}
#[cfg(target_os = "linux")]
fn realtime_allowed() -> bool {
    has_cap_sys_nice() || rtprio_soft() != Some(0)
}
#[cfg(target_os = "linux")]
fn expected_priority() -> u32 {
    match rtprio_soft() {
        Some(0) | None => FIFO_PRIORITY,
        Some(n) => n.min(FIFO_PRIORITY),
    }
}
#[cfg(target_os = "linux")]
fn platform_summary() -> String {
    format!(
        "; RLIMIT_RTPRIO soft={}, CAP_SYS_NICE={}, realtime allowed={}, expected priority={}",
        rtprio_soft().map_or_else(|| "unlimited".to_owned(), |n| n.to_string()),
        has_cap_sys_nice(),
        realtime_allowed(),
        expected_priority()
    )
}
#[cfg(not(target_os = "linux"))]
fn platform_summary() -> String {
    String::new()
}

/// CI raises RLIMIT_RTPRIO before this test and says so; a runner that did
/// not would otherwise pass through the "not allowed" branch unnoticed.
fn require_realtime_if_asked() {
    #[cfg(target_os = "linux")]
    if std::env::var_os("VST_AUDIO_REQUIRE_REALTIME").is_some_and(|value| value == "1") {
        assert!(
            realtime_allowed(),
            "VST_AUDIO_REQUIRE_REALTIME=1, but the CI step failed to raise RLIMIT_RTPRIO \
             (soft={:?}) and CAP_SYS_NICE is absent",
            rtprio_soft()
        );
    }
}

#[cfg(target_os = "macos")]
fn within_one_percent(value: u32, target: u32) -> bool {
    u64::from(value).abs_diff(u64::from(target)) * 100 <= u64::from(target)
}

fn assert_realtime(label: &str, observed: Observed) {
    assert!(
        observed.process_calls > 0,
        "{label}: no block processed; {observed}"
    );
    #[cfg(target_os = "linux")]
    if realtime_allowed() {
        assert_eq!(
            (observed.scheduling, observed.a, observed.b),
            (REALTIME, SCHED_FIFO_RESET_ON_FORK, expected_priority()),
            "{label}: expected SCHED_FIFO|SCHED_RESET_ON_FORK ({SCHED_FIFO_RESET_ON_FORK:#x}) at \
             priority {}; observed {observed}{}",
            expected_priority(),
            platform_summary()
        );
    } else {
        assert_eq!(
            (observed.scheduling, observed.a),
            (NORMAL, SCHED_OTHER),
            "{label}: realtime is not permitted, so the worker must keep SCHED_OTHER; \
             observed {observed}{}",
            platform_summary()
        );
    }
    #[cfg(target_os = "macos")]
    {
        let period = (u64::from(QUANTUM) * 1_000_000_000 / u64::from(SAMPLE_RATE)) as u32;
        assert_eq!(
            observed.scheduling, REALTIME,
            "{label}: expected THREAD_TIME_CONSTRAINT_POLICY; observed {observed} \
             (slot12=0 means thread_policy_get failed with kern_return_t slot13)"
        );
        assert!(
            within_one_percent(observed.a, period),
            "{label}: period {} ns is not within 1% of {period} ns; observed {observed}",
            observed.a
        );
        assert!(
            within_one_percent(observed.c, observed.a),
            "{label}: constraint {} ns is not within 1% of the period {} ns; observed {observed}",
            observed.c,
            observed.a
        );
        assert!(
            0 < observed.b && observed.b <= observed.c,
            "{label}: computation {} ns must be in (0, constraint {} ns]; observed {observed}",
            observed.b,
            observed.c
        );
    }
    #[cfg(windows)]
    assert_eq!(
        observed.scheduling, 0,
        "{label}: the fixture does not observe MMCSS, slot 12 must stay 0; observed {observed}"
    );
}

#[test]
#[ignore = "native fixtures required"]
fn realtime_processing_thread_uses_realtime_scheduling() {
    require_realtime_if_asked();
    let label = "realtime";
    assert_realtime(label, observe(label, &options(false, None)));
}

#[test]
#[ignore = "native fixtures required"]
fn offline_processing_thread_keeps_normal_scheduling() {
    let label = "offline";
    let observed = observe(label, &options(true, None));
    assert!(
        observed.process_calls > 0,
        "{label}: no block processed; {observed}"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        (observed.scheduling, observed.a),
        (NORMAL, SCHED_OTHER),
        "{label}: offline processing must keep SCHED_OTHER; observed {observed}"
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        observed.scheduling, NORMAL,
        "{label}: offline processing must keep the default policy; observed {observed}"
    );
    #[cfg(windows)]
    assert_eq!(
        observed.scheduling, 0,
        "{label}: the fixture does not observe MMCSS, slot 12 must stay 0; observed {observed}"
    );
}

#[test]
#[ignore = "native fixtures required"]
fn sandboxed_realtime_processing_thread_uses_realtime_scheduling() {
    require_realtime_if_asked();
    let label = "sandboxed realtime";
    assert_realtime(
        label,
        observe(label, &options(false, Some(SandboxPolicy::default()))),
    );
}
