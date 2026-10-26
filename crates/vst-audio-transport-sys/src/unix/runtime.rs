#![allow(unsafe_code)]
//! Selected descriptor inheritance. No plugin code runs here.
//!
//! A worker receives only the descriptors named at spawn: macOS spawns with
//! `POSIX_SPAWN_CLOEXEC_DEFAULT`, Linux marks every other descriptor
//! close-on-exec in the fork child before clearing the flag on the selection.
use std::{
    ffi::OsString,
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt, ptr};
#[cfg(not(target_os = "macos"))]
use std::{
    os::unix::process::{CommandExt, ExitStatusExt},
    process::{Child, Command, Stdio},
};

pub fn suppress_error_dialogs() {
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: process-local limit, readable initialized structure. Expected plugin
    // failures must not dump unbounded memory/state or delay worker termination.
    unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) };
}

pub fn duplicate_value(value: usize, _: bool) -> io::Result<OwnedFd> {
    let fd = i32::try_from(value)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid descriptor"))?;
    // SAFETY: fcntl validates the descriptor and returns a new owned descriptor.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful F_DUPFD_CLOEXEC returned unique ownership.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

pub fn inherited_file(value: usize) -> io::Result<File> {
    Ok(duplicate_value(value, false)?.into())
}

/// Both ends are created close-on-exec atomically (`pipe2` on Linux), so a
/// spawn racing on another thread cannot inherit them.
pub(crate) fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let (read, write) = io::pipe()?;
    Ok((read.into(), write.into()))
}

#[derive(Debug)]
pub struct ParentHandle {
    read: OwnedFd,
    _write: OwnedFd,
}

pub fn parent_handle() -> io::Result<ParentHandle> {
    let (read, write) = pipe()?;
    Ok(ParentHandle {
        read,
        _write: write,
    })
}

pub fn parent_value(parent: &ParentHandle) -> usize {
    parent.read.as_raw_fd() as usize
}

pub fn process_gone(parent: &OwnedFd) -> bool {
    let mut fd = libc::pollfd {
        fd: parent.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd; zero timeout never waits.
    unsafe { libc::poll(&mut fd, 1, 0) };
    fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

#[derive(Debug)]
pub struct Pipe {
    pub parent: File,
    pub child: OwnedFd,
}
impl Pipe {
    pub fn create(parent_reads: bool) -> io::Result<Self> {
        let (read, write) = pipe()?;
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
        self.child.as_raw_fd() as usize
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    // Apple <spawn.h>; libc 0.2.191 does not expose this extension.
    fn posix_spawn_file_actions_addinherit_np(
        file_actions: *mut libc::posix_spawn_file_actions_t,
        fd: libc::c_int,
    ) -> libc::c_int;
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct SpawnAttr(libc::posix_spawnattr_t);

#[cfg(target_os = "macos")]
impl SpawnAttr {
    fn new() -> io::Result<Self> {
        let mut raw = MaybeUninit::uninit();
        // SAFETY: raw points to writable storage for one posix_spawnattr_t.
        let result = unsafe { libc::posix_spawnattr_init(raw.as_mut_ptr()) };
        spawn_result(result)?;
        // SAFETY: posix_spawnattr_init succeeded and initialized raw.
        let raw = unsafe { raw.assume_init() };
        Ok(Self(raw))
    }

    fn as_ptr(&self) -> *const libc::posix_spawnattr_t {
        &self.0
    }

    fn as_mut_ptr(&mut self) -> *mut libc::posix_spawnattr_t {
        &mut self.0
    }
}

#[cfg(target_os = "macos")]
impl Drop for SpawnAttr {
    fn drop(&mut self) {
        // SAFETY: self contains an initialized attribute object not yet destroyed.
        let _ = unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct SpawnFileActions(libc::posix_spawn_file_actions_t);

#[cfg(target_os = "macos")]
impl SpawnFileActions {
    fn new() -> io::Result<Self> {
        let mut raw = MaybeUninit::uninit();
        // SAFETY: raw points to writable storage for one file-actions object.
        let result = unsafe { libc::posix_spawn_file_actions_init(raw.as_mut_ptr()) };
        spawn_result(result)?;
        // SAFETY: posix_spawn_file_actions_init succeeded and initialized raw.
        let raw = unsafe { raw.assume_init() };
        Ok(Self(raw))
    }

    fn as_ptr(&self) -> *const libc::posix_spawn_file_actions_t {
        &self.0
    }

    fn as_mut_ptr(&mut self) -> *mut libc::posix_spawn_file_actions_t {
        &mut self.0
    }

    fn add_inherit(&mut self, fd: libc::c_int) -> io::Result<()> {
        // SAFETY: self is initialized and remains alive; fd names the descriptor action.
        let result = unsafe { posix_spawn_file_actions_addinherit_np(&mut self.0, fd) };
        spawn_result(result)
    }
}

#[cfg(target_os = "macos")]
impl Drop for SpawnFileActions {
    fn drop(&mut self) {
        // SAFETY: self contains an initialized file-actions object not yet destroyed.
        let _ = unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

#[cfg(target_os = "macos")]
fn spawn_result(result: libc::c_int) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result))
    }
}

#[cfg(target_os = "macos")]
fn empty_signal_set() -> io::Result<libc::sigset_t> {
    let mut set = MaybeUninit::uninit();
    // SAFETY: set points to writable storage for one sigset_t.
    let result = unsafe { libc::sigemptyset(set.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: sigemptyset succeeded and initialized set.
    Ok(unsafe { set.assume_init() })
}

#[cfg(target_os = "macos")]
fn cstring(bytes: &[u8], field: &str) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{field} contains an interior NUL"),
        )
    })
}

#[cfg(target_os = "macos")]
fn selected_descriptors(values: &[u64]) -> Vec<libc::c_int> {
    let mut descriptors = Vec::new();
    for value in values {
        // Event tokens carry their read/write descriptors in two 32-bit halves.
        descriptors.push((value & 0xffff_ffff) as libc::c_int);
        if value >> 32 != 0 {
            descriptors.push((value >> 32) as libc::c_int);
        }
    }
    descriptors
}

#[cfg(target_os = "macos")]
fn spawn_handles_macos(
    exe: &Path,
    args: &[OsString],
    values: &[u64],
    parent: ParentHandle,
    sandbox: crate::sandbox::SandboxLaunch,
) -> io::Result<ChildProcess> {
    let mut argv_strings = Vec::with_capacity(args.len() + 1);
    argv_strings.push(cstring(exe.as_os_str().as_bytes(), "executable path")?);
    for arg in args {
        argv_strings.push(cstring(arg.as_bytes(), "argument")?);
    }
    let mut argv = argv_strings
        .iter()
        .map(|value| value.as_ptr().cast_mut())
        .collect::<Vec<_>>();
    argv.push(ptr::null_mut());

    let mut environment_strings = Vec::new();
    let temporary = sandbox.temporary();
    for (key, value) in std::env::vars_os()
        .filter(|(key, _)| {
            temporary.is_none() || !["TMPDIR", "TMP", "TEMP"].iter().any(|name| key == name)
        })
        .chain(temporary.into_iter().flat_map(|path| {
            ["TMPDIR", "TMP", "TEMP"].map(|key| (OsString::from(key), path.as_os_str().to_owned()))
        }))
    {
        let mut entry = key.as_bytes().to_vec();
        entry.push(b'=');
        entry.extend_from_slice(value.as_bytes());
        environment_strings.push(cstring(&entry, "environment entry")?);
    }
    let mut environment = environment_strings
        .iter()
        .map(|value| value.as_ptr().cast_mut())
        .collect::<Vec<_>>();
    environment.push(ptr::null_mut());

    let mut attr = SpawnAttr::new()?;
    let empty_mask = empty_signal_set()?;
    // SAFETY: attr and empty_mask are initialized and remain alive for this call.
    let result = unsafe { libc::posix_spawnattr_setsigmask(attr.as_mut_ptr(), &empty_mask) };
    spawn_result(result)?;

    let mut default_signals = empty_signal_set()?;
    // SAFETY: default_signals is initialized and SIGPIPE is a valid signal number.
    let result = unsafe { libc::sigaddset(&mut default_signals, libc::SIGPIPE) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: attr and default_signals are initialized and remain alive for this call.
    let result =
        unsafe { libc::posix_spawnattr_setsigdefault(attr.as_mut_ptr(), &default_signals) };
    spawn_result(result)?;

    // SAFETY: attr is initialized; pgroup zero requests the child's own process group.
    let result = unsafe { libc::posix_spawnattr_setpgroup(attr.as_mut_ptr(), 0) };
    spawn_result(result)?;
    let flags = libc::POSIX_SPAWN_SETPGROUP
        | libc::POSIX_SPAWN_CLOEXEC_DEFAULT
        | libc::POSIX_SPAWN_SETSIGMASK
        | libc::POSIX_SPAWN_SETSIGDEF;
    // SAFETY: attr is initialized and flags contains supported Apple spawn flags.
    let result =
        unsafe { libc::posix_spawnattr_setflags(attr.as_mut_ptr(), flags as libc::c_short) };
    spawn_result(result)?;

    let mut actions = SpawnFileActions::new()?;
    // SAFETY: actions is initialized and /dev/null is a permanent NUL-terminated path.
    let result = unsafe {
        libc::posix_spawn_file_actions_addopen(
            actions.as_mut_ptr(),
            libc::STDIN_FILENO,
            c"/dev/null".as_ptr(),
            libc::O_RDONLY,
            0,
        )
    };
    spawn_result(result)?;
    // SAFETY: actions is initialized and /dev/null is a permanent NUL-terminated path.
    let result = unsafe {
        libc::posix_spawn_file_actions_addopen(
            actions.as_mut_ptr(),
            libc::STDOUT_FILENO,
            c"/dev/null".as_ptr(),
            libc::O_WRONLY,
            0,
        )
    };
    spawn_result(result)?;
    actions.add_inherit(libc::STDERR_FILENO)?;
    for fd in selected_descriptors(values) {
        actions.add_inherit(fd)?;
    }

    let mut pid = 0;
    // SAFETY: all pointers address initialized, NUL-terminated data that outlives the call;
    // attr and actions are initialized, and pid points to writable storage.
    let result = unsafe {
        libc::posix_spawn(
            &mut pid,
            argv_strings[0].as_ptr(),
            actions.as_ptr(),
            attr.as_ptr(),
            argv.as_ptr(),
            environment.as_ptr(),
        )
    };
    spawn_result(result)?;
    Ok(ChildProcess {
        child: Mutex::new(MacChild {
            pid,
            exit_code: None,
        }),
        _parent: parent,
        _sandbox: sandbox,
    })
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct MacChild {
    pid: libc::pid_t,
    exit_code: Option<u32>,
}

#[cfg(target_os = "macos")]
impl MacChild {
    fn cache_status(&mut self, status: libc::c_int) -> u32 {
        let code = if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status) as u32
        } else {
            (128 + libc::WTERMSIG(status)) as u32
        };
        self.exit_code = Some(code);
        code
    }

    fn try_wait(&mut self) -> io::Result<Option<u32>> {
        if self.exit_code.is_some() {
            return Ok(self.exit_code);
        }
        let mut status = 0;
        // SAFETY: self owns this unreaped PID and status points to writable storage.
        let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if result == 0 {
            return Ok(None);
        }
        debug_assert_eq!(result, self.pid);
        Ok(Some(self.cache_status(status)))
    }

    fn wait(&mut self) -> io::Result<u32> {
        if let Some(code) = self.exit_code {
            return Ok(code);
        }
        loop {
            let mut status = 0;
            // SAFETY: self owns this unreaped PID and status points to writable storage.
            let result = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if result == self.pid {
                return Ok(self.cache_status(status));
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

#[derive(Debug)]
pub struct ChildProcess {
    #[cfg(not(target_os = "macos"))]
    child: Mutex<Child>,
    #[cfg(target_os = "macos")]
    child: Mutex<MacChild>,
    // Only the parent retains the write end: death closes it even while a
    // plugin's main thread is blocked inside foreign code.
    _parent: ParentHandle,
    _sandbox: crate::sandbox::SandboxLaunch,
}

pub fn spawn_handles(
    exe: &Path,
    args: &[OsString],
    values: &[u64],
    parent: ParentHandle,
    sandbox: crate::sandbox::SandboxLaunch,
) -> io::Result<ChildProcess> {
    #[cfg(target_os = "macos")]
    {
        spawn_handles_macos(exe, args, values, parent, sandbox)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut descriptors = Vec::new();
        for value in values {
            // Event tokens carry their read/write descriptors in two 32-bit halves.
            descriptors.push((value & 0xffff_ffff) as i32);
            if value >> 32 != 0 {
                descriptors.push((value >> 32) as i32);
            }
        }
        let mut command = Command::new(exe);
        // A compositor-provided socket descriptor is not among the selected
        // descriptors, so its number must not reach the worker as WAYLAND_SOCKET:
        // libwayland would otherwise claim whichever descriptor has that number.
        command.env_remove("WAYLAND_SOCKET");
        if let Some(temporary) = sandbox.temporary() {
            for key in ["TMPDIR", "TMP", "TEMP"] {
                command.env(key, temporary);
            }
        }
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let prepare = move || {
            // SAFETY: async-signal-safe syscall in the fork child, before exec.
            if unsafe { libc::setpgid(0, 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            inherit_only(&descriptors)
        };
        // SAFETY: prepare uses only async-signal-safe syscalls and preallocated data.
        unsafe { command.pre_exec(prepare) };
        let child = command.spawn()?;
        Ok(ChildProcess {
            child: Mutex::new(child),
            _parent: parent,
            _sandbox: sandbox,
        })
    }
}

/// Marks every descriptor from 3 upwards close-on-exec, then clears the flag on
/// the selection. Nothing is closed before exec, so std's own error pipe and the
/// selected descriptors stay valid. Runs in the fork child: raw syscalls only.
#[cfg(not(target_os = "macos"))]
fn inherit_only(descriptors: &[i32]) -> io::Result<()> {
    if !mark_all_cloexec() {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: writable initialized rlimit record; no allocation.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // Kernels without close_range (before 5.11) walk the table instead. The
        // cap keeps an unlimited soft limit from turning this into a long loop.
        let end = limit.rlim_cur.min(1 << 20) as i32;
        for fd in 3..end {
            // SAFETY: flag-only operation; a closed descriptor merely yields EBADF.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
    }
    for fd in descriptors {
        // SAFETY: selected inherited descriptor, no allocation or pointer access.
        if unsafe { libc::fcntl(*fd, libc::F_SETFD, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn mark_all_cloexec() -> bool {
    // SAFETY: scalar-only syscall; CLOSE_RANGE_CLOEXEC changes flags and closes nothing.
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    result == 0
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn mark_all_cloexec() -> bool {
    false
}

impl ChildProcess {
    pub fn pid(&self) -> u32 {
        #[cfg(target_os = "macos")]
        {
            self.child.lock().unwrap_or_else(|e| e.into_inner()).pid as u32
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.child.lock().unwrap_or_else(|e| e.into_inner()).id()
        }
    }
    pub fn exit_code(&self) -> io::Result<Option<u32>> {
        #[cfg(target_os = "macos")]
        {
            self.child
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .try_wait()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(self
                .child
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .try_wait()?
                .map(|s| s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)) as u32))
        }
    }
    /// Waits for confirmed exit, returning `false` only when the timeout expires.
    /// OS errors are returned immediately rather than treated as timeouts.
    pub fn wait_for(&self, timeout: Duration) -> io::Result<bool> {
        let start = Instant::now();
        loop {
            if self.exit_code()?.is_some() {
                return Ok(true);
            }
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Ok(false);
            }
            std::thread::sleep(remaining.min(Duration::from_millis(2)));
        }
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
    pub fn terminate(&self, _: u32) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
            if child.try_wait()?.is_some() {
                return Ok(());
            }
            // SAFETY: spawn put this owned, unreaped child in its own process group.
            // Holding the lock prevents another thread reaping/reusing its PID.
            let _ = unsafe { libc::kill(-child.pid, libc::SIGKILL) };
            // SAFETY: the same owned, unreaped child, whatever process group it has
            // joined since; a worker cannot escape a signal addressed to its PID.
            let result = unsafe { libc::kill(child.pid, libc::SIGKILL) };
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
            if child.try_wait()?.is_some() {
                return Ok(());
            }
            let pid = child.id() as i32;
            // SAFETY: spawn put this owned, unreaped child in its own process group.
            // Holding the lock prevents another thread reaping/reusing its PID.
            let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
            // SAFETY: the same owned, unreaped child, whatever process group it has
            // joined since; a worker cannot escape a signal addressed to its PID.
            let result = unsafe { libc::kill(pid, libc::SIGKILL) };
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.terminate(0);
        #[cfg(target_os = "macos")]
        let _ = self
            .child
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .wait();
        #[cfg(not(target_os = "macos"))]
        let _ = self
            .child
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .wait();
    }
}

/// Best-effort scheduling hook. No elevated scheduling promise on Unix.
#[derive(Debug)]
pub struct Mmcss;
impl Mmcss {
    pub fn pro_audio() -> io::Result<Self> {
        Ok(Self)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn shell_exit(script: &str, values: &[u64]) -> u32 {
        let sandbox = crate::sandbox::SandboxLaunch::new(None, Path::new("/bin/sh")).unwrap();
        let args: Vec<OsString> = ["-c", script].map(OsString::from).into();
        let child = spawn_handles(
            Path::new("/bin/sh"),
            &args,
            values,
            parent_handle().unwrap(),
            sandbox,
        )
        .unwrap();
        assert!(child.wait(Duration::from_secs(10)), "shell did not exit");
        child.exit_code().unwrap().unwrap()
    }

    #[test]
    fn wait_for_preserves_echild_after_external_reap() {
        let sandbox = crate::sandbox::SandboxLaunch::new(None, Path::new("/bin/sh")).unwrap();
        let args = [OsString::from("-c"), OsString::from("exit 0")];
        let child = spawn_handles(
            Path::new("/bin/sh"),
            &args,
            &[],
            parent_handle().unwrap(),
            sandbox,
        )
        .unwrap();
        let pid = child.pid() as libc::pid_t;
        let mut status = 0;
        loop {
            // SAFETY: pid is this test's actual child and status is writable. No
            // other thread uses this ChildProcess; this deliberately reaps it externally.
            let result = unsafe { libc::waitpid(pid, &mut status, 0) };
            if result == pid {
                break;
            }
            let error = io::Error::last_os_error();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        }
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);

        let start = Instant::now();
        let result = child.wait_for(Duration::from_secs(2));
        let elapsed = start.elapsed();
        let legacy = std::panic::catch_unwind(|| child.wait(Duration::ZERO));
        // Drop's try_wait returns ECHILD before signalling, and its blocking wait
        // also returns ECHILD. Do not signal a PID that has already been reaped.
        drop(child);
        let error = result.expect_err("external reap must report ECHILD, not a timeout");
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
        assert!(
            elapsed < Duration::from_secs(1),
            "error was delayed: {elapsed:?}"
        );
        let panic = legacy.expect_err("legacy wait must panic on ECHILD");
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("failed to wait for child process exit"));
    }

    #[test]
    fn only_selected_descriptors_reach_the_child() {
        let file = File::open("/dev/null").unwrap();
        // Leave it inheritable, like a descriptor a foreign library opened.
        // SAFETY: owned descriptor; clearing FD_CLOEXEC has no pointer payload.
        let cleared = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) };
        assert_eq!(cleared, 0);
        let fd = file.as_raw_fd();
        let probe = format!("test -e /proc/self/fd/{fd}");
        assert_ne!(shell_exit(&probe, &[]), 0, "unselected descriptor leaked");
        assert_eq!(
            shell_exit(&probe, &[fd as u64]),
            0,
            "selected descriptor missing"
        );
    }
}
