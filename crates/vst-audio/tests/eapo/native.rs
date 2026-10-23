use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    time::{Duration, Instant},
};
use vst_audio::*;
use vst_audio_test_window::desktop;

#[derive(Clone, Copy, Debug)]
pub enum Case {
    EqVst2,
    EqVst3,
    MverbVst2,
    MverbVst3,
}
impl Case {
    fn format(self) -> PluginFormat {
        match self {
            Self::EqVst2 | Self::MverbVst2 => PluginFormat::Vst2,
            _ => PluginFormat::Vst3,
        }
    }
    fn eq(self) -> bool {
        matches!(self, Self::EqVst2 | Self::EqVst3)
    }
    fn name(self) -> &'static str {
        match self {
            Self::EqVst2 => "eapo_eq_vst2",
            Self::EqVst3 => "eapo_eq_vst3",
            Self::MverbVst2 => "eapo_mverb_vst2",
            Self::MverbVst3 => "eapo_mverb_vst3",
        }
    }
    fn plugin(self) -> &'static str {
        if self.eq() { "3 Band EQ" } else { "MVerb" }
    }
    fn edits(self) -> [((i32, i32), (i32, i32)); 2] {
        if self.eq() {
            [((82, 138), (82, 98)), ((208, 138), (208, 168))]
        } else {
            [((312, 56), (312, 26)), ((184, 56), (184, 86))]
        }
    }
    /// Values set through vst-audio for the reverse direction.
    fn reverse(self) -> &'static [(&'static str, f64)] {
        if self.eq() {
            &[("Low", 0.75), ("Mid", 0.625), ("High", 0.25)]
        } else {
            &[("Decay", 0.75), ("Size", 0.375), ("Mix", 0.625)]
        }
    }
}
fn healthy(event: &HostEvent) {
    assert!(
        !matches!(
            event,
            HostEvent::Failed { .. }
                | HostEvent::RequestFailed { .. }
                | HostEvent::NotificationsLost
                | HostEvent::Stopped
        ),
        "{event:?}"
    );
}
fn request(instance: &Instance, command: Command) -> HostEvent {
    let id = instance.controller().request(command).unwrap();
    until(12, "plugin response", || {
        while let Some(event) = instance.try_next_event() {
            healthy(&event);
            let got = match &event {
                HostEvent::Completed { request } | HostEvent::Parameters { request, .. } => {
                    Some(*request)
                }
                HostEvent::State { request, .. } => *request,
                _ => None,
            };
            if got == Some(id) {
                return Some(event);
            }
        }
        None
    })
}
fn parameters(instance: &Instance) -> Vec<Parameter> {
    let mut result = Vec::new();
    loop {
        let HostEvent::Parameters { total, entries, .. } = request(
            instance,
            Command::ListParameters {
                start: result.len() as u32,
                count: 32,
            },
        ) else {
            panic!("missing parameters")
        };
        assert!(!entries.is_empty() || result.len() == total as usize);
        result.extend(entries);
        if result.len() == total as usize {
            return result;
        }
    }
}
fn set(instance: &Instance, entries: &[Parameter], name: &str, value: f64) {
    let p = entries
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("missing parameter {name}: {entries:?}"));
    assert!(!p.read_only, "read-only parameter {name}");
    assert!(matches!(
        request(instance, Command::SetParameter { id: p.id, value }),
        HostEvent::Completed { .. }
    ));
}
fn component(instance: &Instance) -> Vec<u8> {
    let HostEvent::State { bytes, .. } = request(instance, Command::SaveState) else {
        panic!("missing state")
    };
    let (component, controller) = state::decode(&bytes).unwrap();
    assert!(
        controller.is_empty(),
        "this case has controller state; ChunkData format has not been confirmed"
    );
    component.to_vec()
}
fn until<T>(seconds: u64, label: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(value) = f() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {label}");
        std::thread::sleep(Duration::from_millis(200));
    }
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn directory(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    // Refuse stale artifacts: every invocation needs an empty work root.
    fs::create_dir(&path).unwrap_or_else(|e| panic!("create fresh {}: {e}", path.display()));
    path
}
fn gui(case: Case, app: &Path, root: &Path, library: &Path) -> (PluginLine, usize) {
    let dir = directory(root, &format!("{}-gui", case.name()));
    let config = dir.join("config.txt");
    fs::write(
        &config,
        format!("VSTPlugin: Library \"{}\"\r\n", library.display()),
    )
    .unwrap();
    let mut editor = Process(
        ProcessCommand::new(app.join("Editor.exe"))
            .arg(&config)
            .current_dir(app)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = editor.0.id();
    let main = until(60, "Configuration Editor", || {
        assert!(
            editor.0.try_wait().unwrap().is_none(),
            "Editor exited during startup"
        );
        desktop::top_windows(pid)
            .into_iter()
            .find(|w| w.title.contains("Configuration Editor"))
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut quiet = Instant::now();
    while Instant::now() < deadline {
        let boxes: Vec<_> = desktop::top_windows(pid)
            .into_iter()
            .filter(|w| !w.title.contains("Configuration Editor") && w.class == "Qt6120QWindowIcon")
            .collect();
        if boxes.is_empty() {
            if quiet.elapsed() >= Duration::from_secs(2) {
                break;
            }
        } else {
            quiet = Instant::now();
            for window in boxes {
                if let Some(rect) = desktop::find_named(window.handle, "No", None)
                    .or_else(|| desktop::find_named(window.handle, "OK", None))
                {
                    println!(
                        "METRIC {{\"case\":{:?},\"message_box\":{:?}}}",
                        case.name(),
                        window.title
                    );
                    let (x, y) = rect.center();
                    desktop::click(pid, x, y);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let button = until(15, "Open panel", || {
        desktop::find_named(main.handle, "Open panel", None)
    });
    let (x, y) = button.center();
    desktop::click(pid, x, y);
    let (dialog, plugin) = until(10, "DPF plugin HWND", || {
        desktop::top_windows(pid).into_iter().find_map(|w| {
            desktop::find_child_class(w.handle, &format!("DISTRHO-{}", case.plugin()))
                .map(|p| (w, p))
        })
    });
    let r = plugin.rect;
    println!(
        "METRIC {{\"case\":{:?},\"plugin_rect\":[{},{},{},{}]}}",
        case.name(),
        r.left,
        r.top,
        r.width(),
        r.height()
    );
    // The editor's own pixels show whether the input reached it, apart from
    // whether the host then saved what it did. Wait for its first frames to settle.
    let mut before = desktop::capture(r);
    let settle = Instant::now() + Duration::from_secs(5);
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let again = desktop::capture(r);
        if again == before || Instant::now() >= settle {
            break;
        }
        before = again;
    }
    for (from, to) in case.edits() {
        assert!(
            r.contains(r.left + from.0, r.top + from.1) && r.contains(r.left + to.0, r.top + to.1),
            "plugin coordinates outside {r:?}"
        );
        desktop::drag(
            pid,
            (r.left + from.0, r.top + from.1),
            (r.left + to.0, r.top + to.1),
            12,
        );
        std::thread::sleep(Duration::from_millis(300));
    }
    let after = desktop::capture(r);
    let changed = before
        .as_chunks::<4>()
        .0
        .iter()
        .zip(after.as_chunks::<4>().0)
        .filter(|(a, b)| a != b)
        .count();
    println!(
        "METRIC {{\"case\":{:?},\"editor_pixels_changed\":{changed}}}",
        case.name()
    );
    // Apply first, as a user would before closing the panel.
    if let Some(button) = desktop::find_named(dialog.handle, "Apply", None) {
        let (x, y) = button.center();
        desktop::click(pid, x, y);
        std::thread::sleep(Duration::from_millis(500));
    }
    let button = until(5, "panel OK", || {
        desktop::find_named(dialog.handle, "OK", None)
    });
    let (x, y) = button.center();
    desktop::click(pid, x, y);
    until(5, "panel close", || {
        (!desktop::top_windows(pid)
            .iter()
            .any(|w| w.handle == dialog.handle))
        .then_some(())
    });
    let button = until(5, "Save", || desktop::find_named(main.handle, "Save", None));
    let (x, y) = button.center();
    desktop::click(pid, x, y);
    let line = until(10, "saved plugin state", || {
        fs::read_to_string(&config)
            .ok()?
            .lines()
            .filter(|l| l.trim_start().starts_with("VSTPlugin:"))
            .find_map(|l| {
                let p = parse_line(l).ok()?;
                (if case.format() == PluginFormat::Vst3 {
                    p.chunk.is_some()
                } else {
                    !p.parameters.is_empty()
                })
                .then_some(p)
            })
    });
    editor.0.kill().unwrap();
    editor.0.wait().unwrap();
    assert_eq!(
        Path::new(&line.library),
        library,
        "Editor changed plugin library"
    );
    (line, changed)
}
fn signal(case: Case) -> [Vec<f32>; 2] {
    let frames = ((if case.eq() { 20 } else { 60 }) * RATE).div_ceil(QUANTUM) * QUANTUM;
    let mut out = [Vec::with_capacity(frames), Vec::with_capacity(frames)];
    let mut seeds = [0x12345678u32, 0x9abcdef0u32];
    let log = 1000.0f64.ln();
    let duration = frames as f64 / RATE as f64;
    for i in 0..frames {
        let t = i as f64 / RATE as f64;
        let phase =
            std::f64::consts::TAU * 20.0 * duration / log * ((t * log / duration).exp() - 1.0);
        for ch in 0..2 {
            seeds[ch] = seeds[ch].wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = f64::from(seeds[ch]) / f64::from(u32::MAX) * 2.0 - 1.0;
            let pulse = if i.is_multiple_of(2 * RATE) {
                10.0f64.powf(-6.0 / 20.0)
            } else {
                0.0
            };
            let sample = phase.sin() * 10.0f64.powf(-18.0 / 20.0)
                + noise * 10.0f64.powf(-30.0 / 20.0)
                + pulse;
            assert!(sample.abs() <= 1.0);
            out[ch].push(sample as f32);
        }
    }
    out
}
fn render(instance: &mut Instance, x: &[Vec<f32>; 2]) -> [Vec<f32>; 2] {
    let mut audio = instance.take_audio().unwrap();
    assert_eq!(
        (
            audio.geometry().input_channels,
            audio.geometry().output_channels
        ),
        (2, 2)
    );
    let latency = audio.latency_frames();
    assert_eq!(latency, 2 * QUANTUM, "unexpected plugin latency");
    let total = x[0].len() + latency.div_ceil(QUANTUM) * QUANTUM;
    let mut result = [Vec::with_capacity(total), Vec::with_capacity(total)];
    let silence = [0.0; QUANTUM];
    let mut left = [0.0; QUANTUM];
    let mut right = [0.0; QUANTUM];
    for start in (0..total).step_by(QUANTUM) {
        let input = if start < x[0].len() {
            [&x[0][start..start + QUANTUM], &x[1][start..start + QUANTUM]]
        } else {
            [&silence[..], &silence[..]]
        };
        let status = audio.process(&input, &mut [&mut left, &mut right]);
        // The first blocks are the pipeline's own delay, discarded below.
        if start >= latency {
            assert_eq!(status, AudioStatus::Processed, "block {start}");
        }
        result[0].extend_from_slice(&left);
        result[1].extend_from_slice(&right);
        while let Some(event) = instance.try_next_event() {
            healthy(&event);
        }
        // The endpoint never waits for workers. Pace the offline test on the caller,
        // outside process(), so each two-quantum pipeline deadline is meaningful.
        std::thread::sleep(Duration::from_millis(12));
    }
    let result = result.map(|y| y[latency..latency + x[0].len()].to_vec());
    assert!(
        result
            .iter()
            .flatten()
            .all(|s| s.is_finite() && s.abs() <= 1.0),
        "plugin output clipped/nonfinite"
    );
    result
}
fn db(value: f64) -> f64 {
    20.0 * value.max(1e-15).log10()
}
fn peak(y: &[Vec<f32>; 2]) -> f64 {
    y.iter()
        .flatten()
        .map(|v| f64::from(v.abs()))
        .fold(0.0, f64::max)
}
struct BenchmarkOutput {
    channels: Vec<Vec<i16>>,
    diagnostic: String,
}
fn benchmark(
    app: &Path,
    root: &Path,
    name: &str,
    config: &str,
    channels: &[&[f32]],
) -> BenchmarkOutput {
    let dir = directory(root, name);
    fs::write(dir.join("config.txt"), config).unwrap();
    let input = dir.join("in.wav");
    let output = dir.join("out.wav");
    fs::write(&input, float_wav(channels)).unwrap();
    let log = dir.join("benchmark.stdout");
    let err = dir.join("benchmark.stderr");
    let mut process = Process(
        ProcessCommand::new(app.join("Benchmark.exe"))
            .current_dir(app)
            .arg("--config-path")
            .arg(&dir)
            .arg("-i")
            .arg(input)
            .arg("-o")
            .arg(&output)
            .args(["--batchsize", "512", "--nopause"])
            .stdout(fs::File::create(&log).unwrap())
            .stderr(fs::File::create(&err).unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = process.0.kill();
            let _ = process.0.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let stdout = fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<_> = stdout.lines().collect();
    let diagnostic = format!(
        "Benchmark {name} {status:?}\nstdout (last 20 lines):\n{}",
        lines[lines.len().saturating_sub(20)..].join("\n")
    );
    assert!(status.is_some_and(|s| s.success()), "{diagnostic}");
    let bytes = fs::read(output).unwrap_or_else(|e| panic!("{diagnostic}\n{e}"));
    let channels = pcm16_wav(&bytes, channels.len(), channels[0].len())
        .unwrap_or_else(|e| panic!("{diagnostic}\n{e}"));
    BenchmarkOutput {
        channels,
        diagnostic,
    }
}
fn residual(output: &[Vec<i16>]) -> (f64, f64, f64) {
    let n = output[0].len();
    let sample = |v: i16| f64::from(v) / 32767.0 / 1000.0;
    let max = output[..2]
        .iter()
        .flatten()
        .map(|&v| sample(v).abs())
        .fold(0.0, f64::max);
    let rms = |start: usize| {
        (output[..2]
            .iter()
            .flat_map(|c| &c[start..start + 5 * RATE])
            .map(|&v| sample(v).powi(2))
            .sum::<f64>()
            / (10 * RATE) as f64)
            .sqrt()
    };
    (max, rms(0), rms(n - 5 * RATE))
}
/// Largest difference in LSB between the engine's plain output and `y`, at lag 0.
fn plain_difference(
    app: &Path,
    root: &Path,
    name: &str,
    x: &[Vec<f32>; 2],
    y: &[Vec<f32>; 2],
    line: &str,
) -> f64 {
    let plain = benchmark(app, root, name, &format!("{line}\r\n"), &[&x[0], &x[1]]);
    (0..2)
        .flat_map(|ch| {
            plain.channels[ch]
                .iter()
                .zip(&y[ch])
                .map(|(&o, &v)| (f64::from(o) - f64::from(v) * 32767.0).abs())
        })
        .fold(0.0, f64::max)
}
fn compare_output(
    case: Case,
    direction: &str,
    app: &Path,
    root: &Path,
    x: &[Vec<f32>; 2],
    y: &[Vec<f32>; 2],
    line: &str,
) {
    let name = format!("{}-{direction}", case.name());
    let peak = peak(y);
    assert!(
        peak >= 0.2,
        "{name}: output peak {peak} makes gain control ineffective"
    );
    let plain = benchmark(
        app,
        root,
        &format!("{name}-plain"),
        &format!("{line}\r\n"),
        &[&x[0], &x[1]],
    );
    // Within |y| <= 1 the 32767/32768 scale ambiguity and rounding stay under
    // 1.5 LSB. The lag is searched on the first two seconds (a pulse opens them),
    // then the whole signal is checked at that lag.
    let error = |lag: isize, frames: usize| {
        (0..frames)
            .filter_map(|n| {
                let at = n.checked_add_signed(-lag)?;
                (at < y[0].len()).then(|| {
                    (0..2)
                        .map(|ch| {
                            (f64::from(plain.channels[ch][n]) - f64::from(y[ch][at]) * 32767.0)
                                .abs()
                        })
                        .fold(0.0, f64::max)
                })
            })
            .fold(0.0, f64::max)
    };
    let window = y[0].len().min(2 * RATE);
    let lag = (-64..=64)
        .map(|lag| (lag, error(lag, window)))
        .min_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.abs().cmp(&b.0.abs())))
        .unwrap()
        .0;
    let plain_max = error(lag, y[0].len());
    let config = format!(
        "Channel: 1 2\r\n{line}\r\nCopy: 1=1+-1.0*3 2=2+-1.0*4 3=0.0 4=0.0\r\nChannel: 1 2\r\nPreamp: 60 dB\r\n"
    );
    let output = benchmark(
        app,
        root,
        &format!("{name}-diff"),
        &config,
        &[&x[0], &x[1], &y[0], &y[1]],
    );
    let (max, first, last) = residual(&output.channels);
    let delayed = y.each_ref().map(|c| {
        std::iter::once(0.0)
            .chain(c.iter().copied().take(c.len() - 1))
            .collect::<Vec<_>>()
    });
    let delay_output = benchmark(
        app,
        root,
        &format!("{name}-control-delay"),
        &config,
        &[&x[0], &x[1], &delayed[0], &delayed[1]],
    );
    let delay = residual(&delay_output.channels).0;
    let gained = y.each_ref().map(|c| {
        c.iter()
            .map(|&s| (f64::from(s) * (1.0 + 2e-5)) as f32)
            .collect::<Vec<_>>()
    });
    let gain_output = benchmark(
        app,
        root,
        &format!("{name}-control-gain"),
        &config,
        &[&x[0], &x[1], &gained[0], &gained[1]],
    );
    let gain = residual(&gain_output.channels).0;
    let diagnostics = format!(
        "{}\n{}\n{}\n{}",
        plain.diagnostic, output.diagnostic, delay_output.diagnostic, gain_output.diagnostic
    );
    println!(
        "METRIC {{\"case\":{:?},\"direction\":{direction:?},\"lag\":{lag},\"plain_max_lsb\":{plain_max},\"residual_max_dbfs\":{},\"residual_first_rms_dbfs\":{},\"residual_last_rms_dbfs\":{},\"control_delay_dbfs\":{},\"control_gain_dbfs\":{}}}",
        case.name(),
        db(max),
        db(first),
        db(last),
        db(delay),
        db(gain)
    );
    assert_eq!(lag, 0, "{name}: unexpected host lag\n{diagnostics}");
    assert!(
        plain_max <= 2.0,
        "{name}: plain max {plain_max} LSB\n{diagnostics}"
    );
    assert!(
        max <= 1e-6,
        "{name}: residual {} dBFS\n{diagnostics}",
        db(max)
    );
    assert!(
        last <= (first * 2.0).max(1e-7),
        "{name}: growing residual RMS {first} -> {last}\n{diagnostics}"
    );
    assert!(
        delay > 1e-6 && gain > 1e-6,
        "{name}: insensitive residual controls delay={delay}, gain={gain}\n{diagnostics}"
    );
}
pub fn compare(case: Case) {
    let app = PathBuf::from(std::env::var_os("VST_AUDIO_EAPO").expect("VST_AUDIO_EAPO required"));
    let root = PathBuf::from(
        std::env::var_os("VST_AUDIO_EAPO_WORK").expect("VST_AUDIO_EAPO_WORK required"),
    );
    assert!(app.is_absolute() && root.is_absolute());
    let plugin = super::super::descriptor(if case.eq() { "EQ" } else { "MVERB" }, case.format());
    let library = &plugin.path;
    let configuration = Configuration {
        sample_rate: RATE as u32,
        quantum: QUANTUM as u32,
        ..Default::default()
    };
    let spawn = || {
        super::super::host()
            .spawn(&plugin, &configuration, None)
            .unwrap()
    };
    let x = signal(case);
    let mut default = spawn();
    let defaults = parameters(&default);
    println!(
        "METRIC {{\"case\":{:?},\"parameter_names\":{:?}}}",
        case.name(),
        defaults.iter().map(|p| p.name.as_str()).collect::<Vec<_>>()
    );
    let c0 = (case.format() == PluginFormat::Vst3).then(|| component(&default));
    let y0 = render(&mut default, &x);
    let (saved, changed_pixels) = gui(case, &app, &root, library);
    let mut forward = spawn();
    let saved_edit = if let Some(c0) = &c0 {
        let chunk = saved.chunk.as_ref().expect("missing ChunkData");
        chunk != c0
    } else {
        let changed = saved
            .parameters
            .iter()
            .filter(|(name, value)| {
                let p = defaults
                    .iter()
                    .find(|p| p.name == *name)
                    .unwrap_or_else(|| panic!("unknown saved parameter {name}"));
                (p.value - value).abs() > 1e-3
            })
            .count();
        changed >= 2
    };
    // The editor's pixels show whether the drags reached it; the saved line
    // shows whether the host kept the edit. EQ APO XT up to v2.60.3 lost DPF
    // VST 3 panel edits (EqualizerAPO-XT#437).
    assert!(
        changed_pixels >= 100,
        "the drags did not reach the plugin editor ({changed_pixels} pixels changed)"
    );
    assert!(
        saved_edit,
        "EQ APO XT GUI edit did not change the saved plugin state ({changed_pixels} editor pixels changed)"
    );
    if let Some(chunk) = &saved.chunk {
        restore(&forward, chunk);
    } else {
        for (name, value) in &saved.parameters {
            set(&forward, &defaults, name, *value);
        }
    }
    let y1 = render(&mut forward, &x);
    let change = y0
        .iter()
        .flatten()
        .zip(y1.iter().flatten())
        .map(|(a, b)| f64::from((a - b).abs()))
        .fold(0.0, f64::max);
    println!(
        "METRIC {{\"case\":{:?},\"state\":{:?},\"gui_edits\":2,\"sound_change_db\":{}}}",
        case.name(),
        if saved.chunk.is_some() {
            "chunk"
        } else {
            "parameters"
        },
        db(change)
    );
    assert!(
        change > 1e-3,
        "GUI edit did not change sound by more than -60 dBFS"
    );
    compare_output(case, "forward", &app, &root, &x, &y1, &saved.raw);
    let mut reverse = spawn();
    write_all(&reverse, &parameters(&reverse), case.reverse());
    let library_line = format!("VSTPlugin: Library \"{}\"", library.display());
    let (y2, line) = if case.format() == PluginFormat::Vst3 {
        // VST 3 hands a parameter value to the processor in process(), and a DPF
        // component saves the processor's values: process a few blocks, save,
        // and render from that state restored into a fresh instance, as EQ APO
        // XT loads ChunkData.
        render(
            &mut reverse,
            &[vec![0.0; 8 * QUANTUM], vec![0.0; 8 * QUANTUM]],
        );
        let chunk = component(&reverse);
        let mut restored = spawn();
        restore(&restored, &chunk);
        (
            render(&mut restored, &x),
            format!("{library_line} ChunkData \"{}\"", base64_encode(&chunk)),
        )
    } else {
        let y2 = render(&mut reverse, &x);
        // By name, as the Editor writes them; each value as the shortest decimal
        // that reads back as the plugin's f32.
        let values = parameters(&reverse);
        let by_index: String = values
            .iter()
            .map(|p| format!(" {} {}", p.id, p.value as f32))
            .collect();
        let max = plain_difference(
            &app,
            &root,
            &format!("{}-reverse-by-index", case.name()),
            &x,
            &y2,
            &format!("{library_line}{by_index}"),
        );
        println!(
            "METRIC {{\"case\":{:?},\"observation\":\"VST 2 parameters written by index\",\"plain_max_lsb\":{max}}}",
            case.name()
        );
        (y2, format!("{library_line}{}", named(&values)))
    };
    println!(
        "METRIC {{\"case\":{:?},\"reverse_line\":{line:?}}}",
        case.name()
    );
    compare_output(case, "reverse", &app, &root, &x, &y2, &line);
}
/// Writes every visible, writable parameter, the given ones with their new
/// value, as EQ APO XT does when it loads a VST 2 line. A fresh pinned 3 Band
/// EQ reports a 220 Hz low crossover but filters at 200 Hz until "Low-Mid
/// Freq" is written. A DPF VST 3 processor ignores a write of the value it
/// holds, so for VST 3 only a restored state applies every parameter.
fn write_all(instance: &Instance, entries: &[Parameter], values: &[(&str, f64)]) {
    let writable = |p: &&Parameter| !p.read_only && !p.hidden;
    for (name, _) in values {
        assert!(
            entries.iter().filter(writable).any(|p| p.name == *name),
            "missing writable parameter {name}: {entries:?}"
        );
    }
    for p in entries.iter().filter(writable) {
        let value = values
            .iter()
            .find(|(name, _)| *name == p.name)
            .map_or(p.value, |(_, value)| *value);
        set(instance, entries, &p.name, value);
    }
}
/// Restores a VST 3 component state, which must save back byte for byte.
fn restore(instance: &Instance, chunk: &[u8]) {
    assert!(matches!(
        request(
            instance,
            Command::RestoreState(state::encode(chunk, &[]).unwrap())
        ),
        HostEvent::Completed { .. }
    ));
    let roundtrip = component(instance);
    assert!(
        roundtrip == chunk,
        "component roundtrip mismatch: lengths {} vs {}, first differing byte {:?}",
        roundtrip.len(),
        chunk.len(),
        roundtrip.iter().zip(chunk).position(|(a, b)| a != b)
    );
}
/// `"<name>" <value>` pairs for every parameter, as the Editor writes them.
fn named(parameters: &[Parameter]) -> String {
    parameters
        .iter()
        .map(|p| {
            assert!(!p.name.contains('"'));
            format!(" \"{}\" {}", p.name, p.value as f32)
        })
        .collect()
}
