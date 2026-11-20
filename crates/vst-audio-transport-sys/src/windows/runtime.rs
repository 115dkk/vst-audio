#![allow(unsafe_code)]
//! Owned kernel resources. Handles passed to a child are explicitly selected.
use std::ffi::OsString;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use windows::Win32::{Foundation::*, System::Threading::*};

pub fn suppress_error_dialogs() {
    use windows::Win32::System::Diagnostics::Debug::{
        SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
    };
    // SAFETY: called once during disposable host startup, before plugin threads exist.
    unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX) };
}

pub fn inherited_file(value: usize) -> windows::core::Result<std::fs::File> {
    Ok(duplicate_value(value, false)?.into())
}

pub fn duplicate_value(value: usize, inheritable: bool) -> windows::core::Result<OwnedHandle> {
    let mut result = HANDLE::default();
    // SAFETY: obtains only a pseudo handle, never owned or closed.
    let current = unsafe { GetCurrentProcess() };
    // SAFETY: Windows validates the source handle; result is a fresh owned duplicate.
    unsafe {
        DuplicateHandle(
            current,
            HANDLE(value as *mut _),
            current,
            &mut result,
            0,
            inheritable,
            DUPLICATE_SAME_ACCESS,
        )
    }?;
    Ok(own(result))
}

/// Clears an inherited handle's inherit flag without closing it: the value may be
/// borrowed elsewhere, but a process the plugin starts no longer receives it
/// (audit #11 TD-03).
pub fn stop_inheritance(value: usize) -> windows::core::Result<()> {
    // SAFETY: Windows validates the handle; this changes only its inherit flag.
    unsafe {
        SetHandleInformation(
            HANDLE(value as *mut _),
            HANDLE_FLAG_INHERIT.0,
            HANDLE_FLAGS(0),
        )
    }
}

pub fn parent_handle() -> windows::core::Result<OwnedHandle> {
    win_custody::Process::open(std::process::id(), win_custody::ProcessAccess::Synchronize)
        .map(OwnedHandle::from)
        .map_err(windows::core::Error::from)
}

pub fn process_gone(handle: &OwnedHandle) -> bool {
    // SAFETY: live owned process handle, zero timeout, no pointer payload.
    unsafe { WaitForSingleObject(raw(handle), 0) == WAIT_OBJECT_0 }
}

fn own(handle: HANDLE) -> OwnedHandle {
    // SAFETY: private helper used only for newly created, uniquely owned handles.
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}
fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}
#[derive(Debug)]
pub struct Pipe {
    pub parent: std::fs::File,
    pub child: OwnedHandle,
}
impl Pipe {
    pub fn create(parent_reads: bool) -> windows::core::Result<Self> {
        let (read, write) = win_custody::anonymous_pipe().map_err(windows::core::Error::from)?;
        let (parent, child) = if parent_reads {
            (read, write)
        } else {
            (write, read)
        };
        Ok(Self {
            parent: parent.into(),
            child,
        })
    }
    pub fn child_value(&self) -> usize {
        self.child.as_raw_handle() as usize
    }
}

/// Starts the worker `exe` inside its own kill-on-close job, in an AppContainer
/// when `sandbox` carries a policy. The child receives private copies of the
/// handles in `values` and inherits no other handle. `args` names
/// each handle by its value here, after one of the options in `HANDLE_OPTIONS`,
/// and that value is rewritten to the one the child receives; a value repeated
/// in `values` becomes one copy. Every value must name a handle the caller keeps
/// open through this call. A value after a handle option that is malformed or
/// missing from `values` fails with `E_INVALIDARG` before any process exists.
pub fn spawn_handles(
    exe: &Path,
    args: &[OsString],
    values: &[u64],
    _parent: OwnedHandle,
    sandbox: crate::sandbox::SandboxLaunch,
) -> windows::core::Result<ChildProcess> {
    let profile = crate::sandbox::platform::launch_profile(exe, &sandbox)
        .map_err(windows::core::Error::from)?;
    let mut limits = win_custody::JobLimits::new()
        .kill_on_close(true)
        .die_on_unhandled_exception(true);
    if sandbox
        .policy
        .as_ref()
        .is_some_and(|policy| !policy.processes)
    {
        limits = limits.active_processes(1);
    }
    let job = win_custody::Job::create(&limits).map_err(windows::core::Error::from)?;
    let mut launch = win_custody::Launch::new(exe);
    let mut handles = HandleArguments::default();
    for value in values {
        let original = usize::try_from(*value)
            .map_err(|_| windows::core::Error::from_hresult(E_INVALIDARG))?;
        if handles.mappings.iter().any(|(value, _)| *value == original) {
            continue;
        }
        // SAFETY: the caller guarantees that every value names a live handle it
        // keeps open for the duration of this call.
        let borrowed = unsafe { BorrowedHandle::borrow_raw(original as *mut _) };
        let child = launch
            .inherit(&borrowed)
            .map_err(windows::core::Error::from)?;
        handles.mappings.push((original, child));
    }
    let rewritten = handles.arguments(args)?;
    launch.args(rewritten);
    if let Some(temporary) = sandbox.temporary() {
        for name in ["TMPDIR", "TMP", "TEMP"] {
            launch.env(name, temporary);
        }
    }
    if let Some(profile) = profile.as_ref() {
        launch.app_container(profile);
    }
    let suspended = launch
        .spawn_suspended_in_job(&job)
        .map_err(windows::core::Error::from)?;
    let pid = suspended.pid();
    let process = suspended.resume().map_err(windows::core::Error::from)?;
    Ok(ChildProcess {
        resources: Some(Arc::new(ChildResources {
            _job: job,
            process,
            _sandbox: sandbox,
            #[cfg(test)]
            fail_wait: std::sync::atomic::AtomicBool::new(false),
        })),
        pid,
        #[cfg(test)]
        suppress_termination: false,
    })
}

const HANDLE_OPTIONS: [&str; 7] = [
    "--control-in",
    "--control-out",
    "--region",
    "--work",
    "--done",
    "--ready",
    "--parent",
];

#[derive(Default)]
struct HandleArguments {
    mappings: Vec<(usize, usize)>,
}
impl HandleArguments {
    fn arguments(&self, args: &[OsString]) -> windows::core::Result<Vec<OsString>> {
        let invalid = || windows::core::Error::from_hresult(E_INVALIDARG);
        let mut result = args.to_vec();
        let mut index = 0;
        while index < args.len() {
            if HANDLE_OPTIONS.iter().any(|name| args[index] == *name) {
                index += 1;
                let value = args
                    .get(index)
                    .and_then(|arg| arg.to_str())
                    .ok_or_else(invalid)?;
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(invalid());
                }
                let value: usize = value.parse().map_err(|_| invalid())?;
                let child = self
                    .mappings
                    .iter()
                    .find_map(|(original, child)| (*original == value).then_some(*child))
                    .ok_or_else(invalid)?;
                result[index] = child.to_string().into();
            }
            index += 1;
        }
        Ok(result)
    }
}

#[derive(Debug)]
struct ChildResources {
    _job: win_custody::Job,
    process: win_custody::Process,
    _sandbox: crate::sandbox::SandboxLaunch,
    #[cfg(test)]
    fail_wait: std::sync::atomic::AtomicBool,
}

#[derive(Debug)]
pub struct ChildProcess {
    resources: Option<Arc<ChildResources>>,
    pid: u32,
    #[cfg(test)]
    suppress_termination: bool,
}
impl ChildProcess {
    fn resources(&self) -> &ChildResources {
        self.resources.as_deref().expect("owned child resources")
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn handle(&self) -> HANDLE {
        HANDLE(self.resources().process.as_raw_handle())
    }
    pub fn duplicate_handle(&self) -> std::io::Result<OwnedHandle> {
        self.resources().process.as_handle().try_clone_to_owned()
    }
    /// Waits for confirmed exit, returning `false` only when the timeout expires.
    /// OS errors retain their raw Win32 error code.
    pub fn wait_for(&self, timeout: Duration) -> std::io::Result<bool> {
        self.resources().wait_for(timeout)
    }
    /// Legacy boolean wait: `true` only for a confirmed exit, so an OS error reads
    /// as `false`, as it did before. Use [`Self::wait_for`] to tell the two apart.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.wait_for(timeout).unwrap_or(false)
    }
    pub fn exit_code(&self) -> windows::core::Result<Option<u32>> {
        if !self
            .wait_for(Duration::ZERO)
            .map_err(windows::core::Error::from)?
        {
            return Ok(None);
        }
        self.resources()
            .process
            .exit_code()
            .map_err(windows::core::Error::from)
    }
    pub fn terminate(&self, code: u32) -> windows::core::Result<()> {
        #[cfg(test)]
        if self.suppress_termination {
            return Ok(());
        }
        self.resources()
            .process
            .terminate(code)
            .map_err(windows::core::Error::from)
    }
}

#[deprecated(note = "use RealtimeAudio")]
#[derive(Debug)]
pub struct Mmcss {
    handle: HANDLE,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
#[allow(deprecated)]
impl Mmcss {
    pub fn pro_audio() -> windows::core::Result<Self> {
        let mut index = 0;
        // SAFETY: constant terminated task name and valid task-index pointer.
        let handle =
            unsafe { AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut index) }?;
        Ok(Self {
            handle,
            _thread: std::marker::PhantomData,
        })
    }
}
#[allow(deprecated)]
impl Drop for Mmcss {
    fn drop(&mut self) {
        // SAFETY: registration was created on this thread and the guard is not Send.
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.handle) };
    }
}

/// Cadence of the realtime processing loop a [`RealtimeAudio`] guard serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioPeriod {
    pub frames: u32,
    pub sample_rate: u32,
}

/// Realtime scheduling for the calling thread, restored when dropped.
/// Not `Send`: it must be dropped on the thread that created it.
///
/// Windows registers the thread with the MMCSS "Pro Audio" task. `Debug` shows
/// the registration.
pub struct RealtimeAudio {
    handle: HANDLE,
    /// MMCSS task index the registration received.
    task_index: u32,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl std::fmt::Debug for RealtimeAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeAudio")
            .field("mmcss_task", &"Pro Audio")
            .field("task_index", &self.task_index)
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl RealtimeAudio {
    /// Best effort. An error explains why the OS refused; the caller keeps
    /// processing at normal priority.
    ///
    /// Windows ignores the period beyond validating it: MMCSS "Pro Audio"
    /// schedules by task, not by cadence.
    pub fn promote(period: AudioPeriod) -> std::io::Result<Self> {
        if period.frames == 0 || period.sample_rate == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "audio period needs non-zero frames and sample rate",
            ));
        }
        let mut task_index = 0;
        // SAFETY: constant terminated task name and valid task-index pointer; the
        // registration applies to the calling thread.
        let handle = unsafe {
            AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut task_index)
        }
        .map_err(std::io::Error::from)?;
        Ok(Self {
            handle,
            task_index,
            _thread: std::marker::PhantomData,
        })
    }
}

impl Drop for RealtimeAudio {
    fn drop(&mut self) {
        // SAFETY: the registration was created on this thread and the guard is not
        // Send, so the handle is reverted on the thread that owns it.
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.handle) };
    }
}

pub fn parent_value(parent: &OwnedHandle) -> usize {
    parent.as_raw_handle() as usize
}

impl ChildResources {
    fn wait_for(&self, timeout: Duration) -> std::io::Result<bool> {
        #[cfg(test)]
        if self.fail_wait.swap(false, Ordering::Relaxed) {
            return Err(std::io::Error::from_raw_os_error(
                ERROR_ACCESS_DENIED.0 as i32,
            ));
        }
        match win_custody::wait(&self.process, Some(timeout))? {
            win_custody::WaitOutcome::Signaled => Ok(true),
            win_custody::WaitOutcome::TimedOut => Ok(false),
            win_custody::WaitOutcome::Abandoned => Err(std::io::Error::other(
                "unexpected process wait status: Abandoned",
            )),
        }
    }
}

// Debugger-visible diagnostic: stderr/logging in foreground Drop could block.
static REAPER_SPAWN_FAILURES: AtomicUsize = AtomicUsize::new(0);

impl Drop for ChildProcess {
    fn drop(&mut self) {
        if self.resources.is_none() {
            return;
        }
        let _ = self.terminate(0);
        // Keep the existing bounded foreground wait, off the audio thread.
        if matches!(self.wait_for(Duration::from_secs(5)), Ok(true)) {
            return;
        }
        let resources = self.resources.take().expect("owned child resources");
        let reaper = Arc::clone(&resources);
        // Retain the caller's Arc until spawn succeeds: a rejected spawn drops
        // its closure, which must not release a still-live worker's sandbox.
        if std::thread::Builder::new()
            .name("vst-audio-reaper".into())
            .spawn(move || {
                loop {
                    match reaper.wait_for(Duration::from_secs(5)) {
                        Ok(true) => break,
                        Ok(false) => {}
                        // WAIT_FAILED is not exit confirmation. Keep every
                        // resource and back off rather than spinning on errors.
                        Err(_) => std::thread::sleep(Duration::from_secs(1)),
                    }
                }
            })
            .is_err()
        {
            // No safe cleanup is possible without confirmed exit. Deliberately
            // retain the bundle rather than delete a running worker's storage.
            REAPER_SPAWN_FAILURES.fetch_add(1, Ordering::Relaxed);
            std::mem::forget(resources);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_audio_period_is_invalid_input() {
        for period in [
            AudioPeriod {
                frames: 0,
                sample_rate: 48_000,
            },
            AudioPeriod {
                frames: 128,
                sample_rate: 0,
            },
        ] {
            let error = RealtimeAudio::promote(period).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{error}");
        }
    }

    fn assert_not_inheritable(value: usize) {
        let mut flags = 0;
        // SAFETY: test callers retain the owning resource and flags is writable.
        unsafe { GetHandleInformation(HANDLE(value as *mut _), &mut flags) }.unwrap();
        assert_eq!(flags & HANDLE_FLAG_INHERIT.0, 0);
    }

    #[test]
    fn stop_inheritance_keeps_the_handle_open() {
        let event = crate::Event::create().unwrap();
        let inheritable = duplicate_value(event.handle_value() as usize, true).unwrap();
        let value = inheritable.as_raw_handle() as usize;
        stop_inheritance(value).unwrap();
        assert_not_inheritable(value);
        let token = duplicate_value(event.handle_value() as usize, true).unwrap();
        crate::Event::stop_inheritance(token.as_raw_handle() as u64).unwrap();
        assert_not_inheritable(token.as_raw_handle() as usize);
        // Still a live handle: it can be duplicated again.
        duplicate_value(value, false).unwrap();
    }

    #[test]
    fn app_handles_are_private_and_child_receives_duplicates() {
        use std::io::{Read, Write};
        use vst_audio_ring::{Wait, Waited};

        let mut input = Pipe::create(false).unwrap();
        let mut output = Pipe::create(true).unwrap();
        let region = crate::SharedRegion::create(vst_audio_ring::MIN_REGION_BYTES).unwrap();
        let work = crate::Event::create().unwrap();
        let ready = crate::Event::create().unwrap();
        let parent = parent_handle().unwrap();
        let values = [
            input.child_value() as u64,
            output.child_value() as u64,
            region.handle_value() as u64,
            work.handle_value(),
            work.handle_value(), // Repeated inputs must refer to one duplicate.
            ready.handle_value(),
            parent_value(&parent) as u64,
        ];
        let assert_originals = || {
            for value in values {
                assert_not_inheritable(value as usize);
            }
        };
        assert_originals();
        assert_not_inheritable(input.parent.as_raw_handle() as usize);
        assert_not_inheritable(output.parent.as_raw_handle() as usize);
        let exe = std::env::current_exe().unwrap();
        let mut args: Vec<OsString> = [
            "--exact",
            "runtime::tests::inherited_handle_child",
            "--ignored",
            "--",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        for (name, value) in HANDLE_OPTIONS.into_iter().zip(values) {
            args.extend([name.into(), value.to_string().into()]);
        }
        // Numeric non-handle arguments deliberately collide with an original.
        args.extend([
            "--region-bytes".into(),
            values[0].to_string().into(),
            "--slots".into(),
            values[0].to_string().into(),
            values[0].to_string().into(),
        ]);
        let child = spawn_handles(
            &exe,
            &args,
            &values,
            parent.try_clone().unwrap(),
            crate::sandbox::SandboxLaunch::new(None, &exe).unwrap(),
        )
        .unwrap();
        assert_originals();
        input.parent.write_all(&[73]).unwrap();
        assert!(child.wait_for(Duration::from_secs(15)).unwrap());
        assert_eq!(child.exit_code().unwrap(), Some(0));
        assert_originals();
        drop(output.child);
        let mut response = [0; 10 * 8];
        output.parent.read_exact(&mut response).unwrap();
        let received: Vec<_> = response
            .as_chunks::<8>()
            .0
            .iter()
            .map(|bytes| u64::from_le_bytes(*bytes))
            .collect();
        for (actual, original) in received[..7].iter().zip(values) {
            assert_ne!(*actual, original);
        }
        assert_eq!(received[3], received[4]);
        assert_eq!(&received[7..], &[values[0]; 3]);
        assert_eq!(work.wait(Duration::ZERO), Waited::Signalled);
        assert_eq!(ready.wait(Duration::ZERO), Waited::Signalled);
    }

    #[test]
    #[ignore = "subprocess entry point for app_handles_are_private_and_child_receives_duplicates"]
    fn inherited_handle_child() {
        use std::io::{Read, Write};
        use windows::Win32::System::Memory::*;

        let args: Vec<_> = std::env::args().collect();
        let value = |name: &str| -> usize {
            args[args.iter().position(|arg| arg == name).unwrap() + 1]
                .parse()
                .unwrap()
        };
        let values = HANDLE_OPTIONS.map(value);
        let mut byte = [0];
        inherited_file(values[0])
            .unwrap()
            .read_exact(&mut byte)
            .unwrap();
        assert_eq!(byte, [73]);
        // SAFETY: the parent passed a selected live section handle; map only its first byte.
        let view = unsafe { MapViewOfFile(HANDLE(values[2] as *mut _), FILE_MAP_READ, 0, 0, 1) };
        assert!(!view.Value.is_null());
        // SAFETY: view was just mapped successfully and no references escape it.
        unsafe { UnmapViewOfFile(view) }.unwrap();
        for event in &values[3..6] {
            // SAFETY: these are the selected inherited event handles from the parent.
            unsafe { SetEvent(HANDLE(*event as *mut _)) }.unwrap();
        }
        // SAFETY: selected parent handle carries SYNCHRONIZE and parent is still running.
        let parent_status = unsafe { WaitForSingleObject(HANDLE(values[6] as *mut _), 0) };
        assert_eq!(parent_status, WAIT_TIMEOUT);
        let mut output = inherited_file(values[1]).unwrap();
        for value in values.into_iter().chain([
            value("--region-bytes"),
            value("--slots"),
            args.last().unwrap().parse().unwrap(),
        ]) {
            output.write_all(&(value as u64).to_le_bytes()).unwrap();
        }
    }

    #[test]
    fn handle_arguments_fail_closed_and_preserve_other_values() {
        let event = crate::Event::create().unwrap();
        let original = event.handle_value() as usize;
        let handles = HandleArguments {
            mappings: vec![(original, original + 1)],
        };
        let original = original.to_string();
        let args: Vec<OsString> = [
            "--work",
            &original,
            "--done",
            &original,
            "--slots",
            &original,
            "--region-bytes",
            &original,
            &original,
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let remapped = handles.arguments(&args).unwrap();
        assert_ne!(remapped[1], args[1]);
        assert_eq!(remapped[1], remapped[3]);
        assert_eq!(&remapped[4..], &args[4..]);
        for args in [
            vec!["--work"],
            vec!["--work", "not-a-handle"],
            vec!["--work", "0"],
            vec!["--work", "+42"],
            vec!["--work", "18446744073709551616"],
        ] {
            let args: Vec<_> = args.into_iter().map(Into::into).collect();
            assert_eq!(handles.arguments(&args).unwrap_err().code(), E_INVALIDARG);
        }
    }

    #[test]
    fn spawn_handles_without_handles_still_runs() {
        let exe = std::env::current_exe().unwrap();
        let child = spawn_handles(
            &exe,
            &["--list".into()],
            &[],
            parent_handle().unwrap(),
            crate::sandbox::SandboxLaunch::new(None, &exe).unwrap(),
        )
        .unwrap();
        assert!(child.wait_for(Duration::from_secs(15)).unwrap());
        assert_eq!(child.exit_code().unwrap(), Some(0));
    }

    #[test]
    fn drop_wait_error_retains_temporary_until_reaper_confirms_exit() {
        use std::io::Write;

        let exe = std::env::current_exe().unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().to_owned();
        // Only storage ownership is under test; avoid requiring AppContainer
        // setup for the unit-test executable.
        let sandbox = crate::sandbox::SandboxLaunch {
            policy: None,
            temporary: Some(temporary),
            plugin: exe.clone(),
        };
        let mut gate = Pipe::create(false).unwrap();
        let mut child = spawn_handles(
            &exe,
            &[
                "--exact".into(),
                "runtime::tests::reaper_wait_child".into(),
                "--ignored".into(),
                "--".into(),
                "--control-in".into(),
                gate.child_value().to_string().into(),
            ],
            &[gate.child_value() as u64],
            parent_handle().unwrap(),
            sandbox,
        )
        .unwrap();
        let process = child.duplicate_handle().unwrap();
        // The child cannot exit normally until this test releases the pipe.
        // Suppress termination to model a delayed kill; fail the actual wait
        // boundary once so Drop must hand ownership to the background reaper.
        child.suppress_termination = true;
        child.resources().fail_wait.store(true, Ordering::Relaxed);
        drop(child);
        assert!(!process_gone(&process));
        assert!(path.exists(), "Drop removed storage before confirmed exit");

        gate.parent.write_all(&[1]).unwrap();
        // SAFETY: this independently owned duplicate retains SYNCHRONIZE rights.
        let status = unsafe { WaitForSingleObject(raw(&process), 15_000) };
        assert_eq!(status, WAIT_OBJECT_0);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !path.exists(),
            "reaper did not remove storage after child exit"
        );
    }

    #[test]
    #[ignore = "subprocess entry point for drop_wait_error_retains_temporary_until_reaper_confirms_exit"]
    fn reaper_wait_child() {
        use std::io::Read;

        let args: Vec<_> = std::env::args().collect();
        let index = args.iter().position(|arg| arg == "--control-in").unwrap();
        let handle = args[index + 1].parse().unwrap();
        let mut byte = [0];
        inherited_file(handle)
            .unwrap()
            .read_exact(&mut byte)
            .unwrap();
        assert_eq!(byte, [1]);
    }

    #[test]
    fn process_wait_errors_preserve_access_denied() {
        let pid = std::process::id();
        let process =
            win_custody::Process::open(pid, win_custody::ProcessAccess::QueryLimited).unwrap();
        let job = win_custody::Job::create(&win_custody::JobLimits::new()).unwrap();
        let mut child = ChildProcess {
            resources: Some(Arc::new(ChildResources {
                _job: job,
                process,
                _sandbox: crate::sandbox::SandboxLaunch::new(None, Path::new("unused")).unwrap(),
                fail_wait: std::sync::atomic::AtomicBool::new(false),
            })),
            pid,
            suppress_termination: false,
        };

        let error = child.wait_for(Duration::ZERO).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED.0 as i32));
        let error = child.exit_code().unwrap_err();
        assert_eq!(
            error.code(),
            windows::core::HRESULT::from_win32(ERROR_ACCESS_DENIED.0)
        );
        // The supervisor calls the legacy wait; an OS error must not panic it.
        assert!(!child.wait(Duration::ZERO));
        // This artificial object owns no child: it only has a query handle to
        // ourselves and an empty job. Do not start an endless reaper for self.
        drop(child.resources.take());
    }
}
