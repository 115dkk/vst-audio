#![allow(unsafe_code)]
//! Owned kernel resources. Handles passed to a child are explicitly selected.
use std::ffi::{OsStr, OsString};
use std::os::windows::{
    ffi::{OsStrExt, OsStringExt},
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use windows::{
    Win32::{
        Foundation::*,
        Security::SECURITY_ATTRIBUTES,
        System::{JobObjects::*, Pipes::CreatePipe, Threading::*},
    },
    core::{PCWSTR, PWSTR},
};

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

pub fn parent_handle() -> windows::core::Result<OwnedHandle> {
    // SAFETY: obtains only the current process's pseudo handle.
    let current = unsafe { GetCurrentProcess() };
    let mut handle = HANDLE::default();
    // SAFETY: live current process, writable output, only wait/synchronize rights.
    // The worker observes parent death but must not inherit a full-access handle.
    unsafe {
        DuplicateHandle(
            current,
            current,
            current,
            &mut handle,
            PROCESS_SYNCHRONIZE.0,
            false,
            DUPLICATE_HANDLE_OPTIONS(0),
        )
    }?;
    Ok(own(handle))
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
fn wide(value: &OsStr) -> windows::core::Result<Vec<u16>> {
    let mut value: Vec<u16> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(windows::core::Error::from_hresult(E_INVALIDARG));
    }
    value.push(0);
    Ok(value)
}

#[derive(Debug)]
pub struct Pipe {
    pub parent: std::fs::File,
    pub child: OwnedHandle,
}
impl Pipe {
    pub fn create(parent_reads: bool) -> windows::core::Result<Self> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            bInheritHandle: false.into(),
            ..Default::default()
        };
        let mut read = HANDLE::default();
        let mut write = HANDLE::default();
        // SAFETY: valid out pointers and non-inheritable attributes for both ends.
        unsafe { CreatePipe(&mut read, &mut write, Some(&sa), 0) }?;
        let (read, write) = (own(read), own(write));
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

struct Attributes<'a> {
    _storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    _values: std::marker::PhantomData<(
        &'a [HANDLE],
        &'a windows::Win32::Security::SECURITY_CAPABILITIES,
    )>,
}
impl<'a> Attributes<'a> {
    fn new(
        handles: &'a [HANDLE],
        capabilities: Option<&'a windows::Win32::Security::SECURITY_CAPABILITIES>,
    ) -> windows::core::Result<Self> {
        let count = u32::from(!handles.is_empty()) + u32::from(capabilities.is_some());
        let mut bytes = 0;
        // SAFETY: null list is the documented size-query call; bytes is writable.
        let _ = unsafe { InitializeProcThreadAttributeList(None, count, Some(0), &mut bytes) };
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        // SAFETY: aligned backing storage is at least the queried size and remains live.
        unsafe { InitializeProcThreadAttributeList(Some(list), count, Some(0), &mut bytes) }?;
        let attributes = Self {
            _storage: storage,
            list,
            _values: std::marker::PhantomData,
        };
        if !handles.is_empty() {
            // SAFETY: list is initialized; the borrow keeps the handle array live
            // until DeleteProcThreadAttributeList, as UpdateProcThreadAttribute requires.
            unsafe {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    Some(handles.as_ptr().cast()),
                    size_of_val(handles),
                    None,
                    None,
                )
            }?;
        }
        if let Some(capabilities) = capabilities {
            // SAFETY: capability struct and its SID remain live through process creation.
            unsafe {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                    Some(
                        (capabilities as *const windows::Win32::Security::SECURITY_CAPABILITIES)
                            .cast(),
                    ),
                    size_of_val(capabilities),
                    None,
                    None,
                )
            }?;
        }
        Ok(attributes)
    }
}
impl Drop for Attributes<'_> {
    fn drop(&mut self) {
        // SAFETY: initialized list is released before backing storage.
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

pub fn spawn_handles(
    exe: &Path,
    args: &[OsString],
    values: &[u64],
    _parent: OwnedHandle,
    sandbox: crate::sandbox::SandboxLaunch,
) -> windows::core::Result<ChildProcess> {
    let handles = values
        .iter()
        .map(|value| {
            usize::try_from(*value)
                .map(|value| HANDLE(value as *mut _))
                .map_err(|_| windows::core::Error::from_hresult(E_INVALIDARG))
        })
        .collect::<windows::core::Result<Vec<_>>>()?;
    ChildProcess::spawn_inner(exe, args, &handles, sandbox, true)
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

struct InheritedDuplicates {
    originals: Vec<usize>,
    owned: Vec<OwnedHandle>,
}
impl InheritedDuplicates {
    fn new(handles: &[HANDLE]) -> windows::core::Result<Self> {
        let mut result = Self {
            originals: Vec::new(),
            owned: Vec::new(),
        };
        for handle in handles {
            let value = handle.0 as usize;
            if !result.originals.contains(&value) {
                result.owned.push(duplicate_value(value, true)?);
                result.originals.push(value);
            }
        }
        Ok(result)
    }

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
                let position = self
                    .originals
                    .iter()
                    .position(|original| *original == value)
                    .ok_or_else(invalid)?;
                result[index] = (self.owned[position].as_raw_handle() as usize)
                    .to_string()
                    .into();
            }
            index += 1;
        }
        Ok(result)
    }
}

#[derive(Debug)]
struct ChildResources {
    job: OwnedHandle,
    process: OwnedHandle,
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

    /// Low-level launch preserving the supplied handle identities and arguments.
    /// Every selected handle must already be inheritable and remain live through
    /// this call. Use `spawn_handles` for the worker CLI and private app resources.
    pub fn spawn(
        exe: &Path,
        args: &[OsString],
        inherit: &[HANDLE],
        sandbox: crate::sandbox::SandboxLaunch,
    ) -> windows::core::Result<Self> {
        Self::spawn_inner(exe, args, inherit, sandbox, false)
    }

    fn spawn_inner(
        exe: &Path,
        args: &[OsString],
        inherit: &[HANDLE],
        sandbox: crate::sandbox::SandboxLaunch,
        duplicate: bool,
    ) -> windows::core::Result<Self> {
        let security = crate::sandbox::platform::LaunchSecurity::new(exe, &sandbox)?;
        let exe = wide(exe.as_os_str())?;
        // SAFETY: unnamed job, no inheritable attributes.
        let job = own(unsafe { CreateJobObjectW(None, None) }?);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        if sandbox
            .policy
            .as_ref()
            .is_some_and(|policy| !policy.processes)
        {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            limits.BasicLimitInformation.ActiveProcessLimit = 1;
        }
        // SAFETY: limits matches the requested information class and byte size.
        unsafe {
            SetInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
            )
        }?;
        let mut info = PROCESS_INFORMATION::default();
        let environment = sandbox.temporary().map(environment).transpose()?;
        let environment = environment.as_ref().map(|block| block.as_ptr().cast());
        // The worker is a console program. Launched from a GUI application, which has
        // no console, Windows would open a new console window for every instance.
        let flags = EXTENDED_STARTUPINFO_PRESENT
            | CREATE_SUSPENDED
            | CREATE_UNICODE_ENVIRONMENT
            | CREATE_NO_WINDOW;
        // Only the worker path duplicates: low-level callers may communicate handle
        // identities outside argv. Originals are never made inheritable here.
        // An unrelated concurrent CreateProcess with unrestricted inheritance can
        // still acquire these temporary duplicates during this short window.
        let duplicates = duplicate
            .then(|| InheritedDuplicates::new(inherit))
            .transpose()?;
        let remapped = duplicates
            .as_ref()
            .map(|handles| handles.arguments(args))
            .transpose()?;
        let args = remapped.as_deref().unwrap_or(args);
        let mut command = OsString::from("\"vst-audio-worker\"");
        for arg in args {
            command.push(" ");
            // Windows CommandLineToArgvW quoting, including trailing backslashes.
            command.push(quote_argument(arg)?);
        }
        let mut command = wide(&command)?;
        let handles = duplicates
            .as_ref()
            .map(|handles| handles.owned.iter().map(raw).collect::<Vec<_>>());
        let inherit = handles.as_deref().unwrap_or(inherit);
        let attributes = if inherit.is_empty() && security.capabilities.is_none() {
            None
        } else {
            Some(Attributes::new(inherit, security.capabilities.as_ref())?)
        };
        let startup = STARTUPINFOEXW {
            StartupInfo: STARTUPINFOW {
                cb: size_of::<STARTUPINFOEXW>() as u32,
                ..Default::default()
            },
            lpAttributeList: attributes
                .as_ref()
                .map_or(LPPROC_THREAD_ATTRIBUTE_LIST::default(), |attributes| {
                    attributes.list
                }),
        };
        // SAFETY: valid terminated strings/environment and extended startup with
        // optional AppContainer capability SID; child is suspended until job assignment.
        let created = unsafe {
            CreateProcessW(
                PCWSTR(exe.as_ptr()),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                !inherit.is_empty(),
                flags,
                environment,
                None,
                &startup.StartupInfo,
                &mut info,
            )
        };
        // The attribute values must outlive DeleteProcThreadAttributeList. Close
        // every temporary inheritable handle before job assignment or error return.
        drop(attributes);
        drop(handles);
        drop(duplicates);
        created?;
        let process = own(info.hProcess);
        let thread = own(info.hThread);
        let child = Self {
            resources: Some(Arc::new(ChildResources {
                job,
                process,
                _sandbox: sandbox,
                #[cfg(test)]
                fail_wait: std::sync::atomic::AtomicBool::new(false),
            })),
            pid: info.dwProcessId,
            #[cfg(test)]
            suppress_termination: false,
        };
        // SAFETY: child is still suspended and both handles remain owned.
        let assigned = unsafe {
            AssignProcessToJobObject(raw(&child.resources().job), raw(&child.resources().process))
        };
        if let Err(error) = assigned {
            let _ = child.terminate(3);
            return Err(error);
        }
        // SAFETY: this is the live primary thread created suspended above.
        if unsafe { ResumeThread(raw(&thread)) } == u32::MAX {
            let error = windows::core::Error::from_thread();
            let _ = child.terminate(3);
            return Err(error);
        }
        Ok(child)
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn handle(&self) -> HANDLE {
        raw(&self.resources().process)
    }
    pub fn duplicate_handle(&self) -> std::io::Result<OwnedHandle> {
        self.resources().process.try_clone()
    }
    /// Waits for confirmed exit, returning `false` only when the timeout expires.
    /// OS errors retain their raw Win32 error code.
    pub fn wait_for(&self, timeout: Duration) -> std::io::Result<bool> {
        self.resources().wait_for(timeout)
    }
    /// Legacy boolean wait; use [`Self::wait_for`] to handle OS errors.
    ///
    /// # Panics
    ///
    /// Panics if waiting fails. This fail-loud policy no longer disguises an OS
    /// error as a timeout.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.wait_for(timeout)
            .unwrap_or_else(|error| panic!("failed to wait for child process exit: {error}"))
    }
    pub fn exit_code(&self) -> windows::core::Result<Option<u32>> {
        if !self
            .wait_for(Duration::ZERO)
            .map_err(windows::core::Error::from)?
        {
            return Ok(None);
        }
        let mut code = 0;
        // SAFETY: live process handle and valid out pointer; process has exited.
        unsafe { GetExitCodeProcess(self.handle(), &mut code) }?;
        Ok(Some(code))
    }
    pub fn terminate(&self, code: u32) -> windows::core::Result<()> {
        #[cfg(test)]
        if self.suppress_termination {
            return Ok(());
        }
        // SAFETY: owned child process handle carries terminate access.
        unsafe { TerminateProcess(self.handle(), code) }
    }
}

#[derive(Debug)]
pub struct Mmcss {
    handle: HANDLE,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
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
impl Drop for Mmcss {
    fn drop(&mut self) {
        // SAFETY: registration was created on this thread and the guard is not Send.
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
        // SAFETY: owned process handle remains valid for the whole wait.
        let result = unsafe {
            WaitForSingleObject(
                raw(&self.process),
                timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32,
            )
        };
        match result {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => Err(std::io::Error::last_os_error()),
            status => Err(std::io::Error::other(format!(
                "unexpected process wait status: {status:?}"
            ))),
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
fn quote_argument(argument: &OsStr) -> windows::core::Result<OsString> {
    let units: Vec<u16> = argument.encode_wide().collect();
    if units.contains(&0) {
        return Err(windows::core::Error::from_hresult(E_INVALIDARG));
    }
    let mut result = vec![b'"' as u16];
    let mut slashes = 0;
    for unit in units {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            b'\\' as u16,
            if unit == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        result.push(unit);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    result.push(b'"' as u16);
    Ok(OsString::from_wide(&result))
}
fn environment(temporary: &Path) -> windows::core::Result<Vec<u16>> {
    let mut entries: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| {
            !["TMPDIR", "TMP", "TEMP"]
                .iter()
                .any(|name| key.to_string_lossy().eq_ignore_ascii_case(name))
        })
        .collect();
    entries.extend(
        ["TMPDIR", "TMP", "TEMP"]
            .map(|key| (OsString::from(key), temporary.as_os_str().to_owned())),
    );
    entries.sort_by_key(|(key, _)| key.to_string_lossy().to_uppercase());
    let mut result = Vec::new();
    for (mut key, value) in entries {
        key.push("=");
        key.push(value);
        result.extend(wide(&key)?);
    }
    result.push(0);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_not_inheritable(value: usize) {
        let mut flags = 0;
        // SAFETY: test callers retain the owning resource and flags is writable.
        unsafe { GetHandleInformation(HANDLE(value as *mut _), &mut flags) }.unwrap();
        assert_eq!(flags & HANDLE_FLAG_INHERIT.0, 0);
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
        let duplicates =
            InheritedDuplicates::new(&[HANDLE(original as *mut _), HANDLE(original as *mut _)])
                .unwrap();
        assert_eq!(duplicates.owned.len(), 1);
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
        let remapped = duplicates.arguments(&args).unwrap();
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
            assert_eq!(
                duplicates.arguments(&args).unwrap_err().code(),
                E_INVALIDARG
            );
        }
    }

    #[test]
    fn direct_spawn_without_handles_still_runs() {
        let exe = std::env::current_exe().unwrap();
        let child = ChildProcess::spawn(
            &exe,
            &["--list".into()],
            &[],
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
        // SAFETY: the current process exists; OpenProcess returns a fresh owned
        // handle with query rights only, deliberately excluding SYNCHRONIZE.
        let process =
            own(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.unwrap());
        // SAFETY: unnamed job with default security; no process is assigned to it.
        let job = own(unsafe { CreateJobObjectW(None, None) }.unwrap());
        let mut child = ChildProcess {
            resources: Some(Arc::new(ChildResources {
                job,
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
        let panic = std::panic::catch_unwind(|| child.wait(Duration::ZERO)).unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("failed to wait for child process exit"));
        // This artificial object owns no child: it only has a query handle to
        // ourselves and an empty job. Do not start an endless reaper for self.
        drop(child.resources.take());
    }
}
