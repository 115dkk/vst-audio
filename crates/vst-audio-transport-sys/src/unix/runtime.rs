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
    sync::{Arc, Mutex},
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

/// Marks an inherited descriptor close-on-exec without closing it: the number may
/// be borrowed elsewhere, but a process the plugin starts no longer receives it
/// (audit #11 TD-03).
pub fn stop_inheritance(value: usize) -> io::Result<()> {
    let fd = i32::try_from(value)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid descriptor"))?;
    // SAFETY: fcntl validates the descriptor and only reads its flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: flag-only change on the validated descriptor; nothing is closed.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
        child: Mutex::new(Some(UnixChild {
            pid,
            exit_code: None,
            ownership_lost: false,
        })),
        _parent: parent,
        sandbox: Some(sandbox),
        #[cfg(test)]
        suppress_termination: false,
    })
}

#[derive(Debug)]
struct UnixChild {
    #[cfg(not(target_os = "macos"))]
    child: Child,
    #[cfg(target_os = "macos")]
    pid: libc::pid_t,
    exit_code: Option<u32>,
    ownership_lost: bool,
}

impl UnixChild {
    fn pid(&self) -> libc::pid_t {
        #[cfg(target_os = "macos")]
        {
            self.pid
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.child.id() as libc::pid_t
        }
    }

    // Called under ChildProcess's mutex, or by the sole owner in the reaper.
    // WNOWAIT keeps even an exited leader's PID reserved until group cleanup.
    fn exit_pending(&mut self) -> io::Result<bool> {
        if self.ownership_lost {
            return Err(io::Error::from_raw_os_error(libc::ECHILD));
        }
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: info is writable siginfo_t storage, and P_PID selects only this
        // child's positive PID. WNOWAIT never consumes its wait status.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.pid() as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            self.ownership_lost = error.raw_os_error() == Some(libc::ECHILD);
            return Err(error);
        }
        // SAFETY: the storage was zeroed and waitid succeeded; the zero PID also
        // covers systems that leave the structure unchanged when no exit is pending.
        let info = unsafe { info.assume_init() };
        // SAFETY: successful waitid with WEXITED supplies the SIGCHLD layout.
        Ok(unsafe { info.si_pid() } != 0)
    }

    fn kill_group(&self) -> io::Result<()> {
        // SAFETY: spawn created a group led by this child, and the caller just
        // validated outstanding wait ownership without reaping. Exclusive access
        // prevents our other wait paths from releasing the PID before this signal.
        let result = unsafe { libc::kill(-self.pid(), libc::SIGKILL) };
        if result != 0 {
            let error = io::Error::last_os_error();
            // ESRCH: the group is empty. EPERM: nothing in it can be signalled,
            // which Darwin reports for a group of zombies, the usual state once
            // the leader exited. Either way the leader must still be reaped.
            if !matches!(error.raw_os_error(), Some(libc::ESRCH | libc::EPERM)) {
                return Err(error);
            }
        }
        Ok(())
    }

    fn try_wait(&mut self) -> io::Result<Option<u32>> {
        if self.exit_code.is_some() {
            return Ok(self.exit_code);
        }
        if !self.exit_pending()? {
            return Ok(None);
        }
        // Audit #11 TD-45: a plugin helper can outlive its leader. Kill the group before
        // every reap, including supervision and the reaper; see
        // drop_removes_helper_after_worker_leader_exits at the Instance seam.
        self.kill_group()?;
        #[cfg(target_os = "macos")]
        let code = {
            let mut status = 0;
            // SAFETY: status is writable and the non-reaping probe confirmed this
            // child exited. Exclusive ownership is held through group kill and reap.
            let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
            if result < 0 {
                let error = io::Error::last_os_error();
                self.ownership_lost = error.raw_os_error() == Some(libc::ECHILD);
                return Err(error);
            }
            if result == 0 {
                return Ok(None);
            }
            if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status) as u32
            } else {
                (128 + libc::WTERMSIG(status)) as u32
            }
        };
        #[cfg(not(target_os = "macos"))]
        let code = match self.child.try_wait() {
            Ok(Some(status)) => status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
                as u32,
            Ok(None) => return Ok(None),
            Err(error) => {
                self.ownership_lost = error.raw_os_error() == Some(libc::ECHILD);
                return Err(error);
            }
        };
        self.exit_code = Some(code);
        Ok(Some(code))
    }

    fn terminate(&mut self) -> io::Result<()> {
        if self.exit_code.is_some() {
            return Ok(());
        }
        self.exit_pending()?;
        let group_result = self.kill_group();
        // SAFETY: waitid confirmed outstanding child ownership without reaping;
        // exclusive access keeps its PID reserved even if it exits during the call.
        // Address the leader too in case it has moved out of its original group.
        let result = unsafe { libc::kill(self.pid(), libc::SIGKILL) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        group_result
    }
}

#[derive(Debug)]
pub struct ChildProcess {
    child: Mutex<Option<UnixChild>>,
    // Only the parent retains the write end: death closes it even while a
    // plugin's main thread is blocked inside foreign code.
    _parent: ParentHandle,
    sandbox: Option<crate::sandbox::SandboxLaunch>,
    #[cfg(test)]
    suppress_termination: bool,
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
        spawn_handles_unix(
            exe,
            args,
            values,
            parent,
            sandbox,
            #[cfg(test)]
            false,
        )
    }
}

#[cfg(not(target_os = "macos"))]
fn spawn_handles_unix(
    exe: &Path,
    args: &[OsString],
    values: &[u64],
    parent: ParentHandle,
    sandbox: crate::sandbox::SandboxLaunch,
    #[cfg(test)] force_fallback: bool,
) -> io::Result<ChildProcess> {
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
    // Preparation errors matter only if close_range is unavailable in the child.
    let fallback =
        prepare_cloexec_fallback().map_err(|error| error.raw_os_error().unwrap_or(libc::EIO));
    let prepare = move || {
        // SAFETY: async-signal-safe syscall in the fork child, before exec.
        if unsafe { libc::setpgid(0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        inherit_only(
            &descriptors,
            &fallback,
            #[cfg(test)]
            force_fallback,
        )
    };
    // SAFETY: prepare uses only async-signal-safe syscalls and preallocated data.
    unsafe { command.pre_exec(prepare) };
    let child = command.spawn()?;
    Ok(ChildProcess {
        child: Mutex::new(Some(UnixChild {
            child,
            exit_code: None,
            ownership_lost: false,
        })),
        _parent: parent,
        sandbox: Some(sandbox),
        #[cfg(test)]
        suppress_termination: false,
    })
}

#[cfg(not(target_os = "macos"))]
struct CloexecFallback {
    end: i32,
    high: Vec<i32>,
}

#[cfg(not(target_os = "macos"))]
fn descriptor_limit() -> io::Result<libc::rlimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limit points to writable initialized storage for one rlimit.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(limit)
}

#[cfg(target_os = "linux")]
fn prepare_cloexec_fallback() -> io::Result<CloexecFallback> {
    // A soft limit can be raised by another thread. The hard limit covers new
    // descriptors, including std's error pipe created after this snapshot.
    let end = i32::try_from(descriptor_limit()?.rlim_max)
        .map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    let mut high = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd")? {
        let name = entry?.file_name();
        let fd = name
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
            .filter(|fd| *fd >= 0)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
        if fd >= end && fd >= 3 {
            high.push(fd);
        }
    }
    // Audit #11 TD-44: preexisting fds survive even lowering the hard limit. Keep them
    // separately; see fallback_excludes_descriptor_opened_before_limit_was_lowered.
    Ok(CloexecFallback { end, high })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn prepare_cloexec_fallback() -> io::Result<CloexecFallback> {
    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
}

/// Marks every descriptor from 3 upwards close-on-exec, then clears the flag on
/// the selection. Nothing is closed before exec, so std's own error pipe and the
/// selected descriptors stay valid. Runs in the fork child: raw syscalls only.
#[cfg(not(target_os = "macos"))]
fn inherit_only(
    descriptors: &[i32],
    fallback: &Result<CloexecFallback, i32>,
    #[cfg(test)] force_fallback: bool,
) -> io::Result<()> {
    #[cfg(not(test))]
    let force_fallback = false;
    if force_fallback || !mark_all_cloexec() {
        let fallback = fallback
            .as_ref()
            .map_err(|error| io::Error::from_raw_os_error(*error))?;
        // No parent threads remain in this fork child's descriptor table. Refuse
        // a raised hard limit: the prepared coverage no longer proves completeness.
        // This covers ordinary concurrent opens/dups and soft-limit changes, not
        // privileged peers raising then restoring the hard limit during preparation.
        if descriptor_limit()?.rlim_max > fallback.end as libc::rlim_t {
            return Err(io::Error::from_raw_os_error(libc::EAGAIN));
        }
        for fd in (3..fallback.end).chain(fallback.high.iter().copied()) {
            if let Err(error) = set_cloexec(fd, true)
                && error.raw_os_error() != Some(libc::EBADF)
            {
                return Err(error);
            }
        }
    }
    for fd in descriptors {
        set_cloexec(*fd, false)?;
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn set_cloexec(fd: i32, cloexec: bool) -> io::Result<()> {
    // SAFETY: scalar-only query; fcntl validates fd, including unselected holes.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let flags = if cloexec {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    // SAFETY: flag-only operation on a queried fd, with every other flag preserved.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
        return Err(io::Error::last_os_error());
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
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .unwrap()
            .pid() as u32
    }
    pub fn exit_code(&self) -> io::Result<Option<u32>> {
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .unwrap()
            .try_wait()
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
    /// Legacy boolean wait: `true` only for a confirmed exit, so an OS error reads
    /// as `false`, as it did before. Use [`Self::wait_for`] to tell the two apart.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.wait_for(timeout).unwrap_or(false)
    }
    pub fn terminate(&self, _: u32) -> io::Result<()> {
        #[cfg(test)]
        if self.suppress_termination {
            return Ok(());
        }
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .unwrap()
            .terminate()
    }
}

// Debugger-visible diagnostic for the last-resort leak below. Logging to stderr
// in Drop could itself block indefinitely; this counter performs no I/O.
static REAPER_SPAWN_FAILURES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.terminate(0);
        match self.wait_for(Duration::from_millis(100)) {
            Ok(true) => return,
            // An external waiter has already reaped the child. Never signal that
            // PID again; it may now belong to an unrelated process.
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => return,
            Ok(false) | Err(_) => {}
        }
        // Only Drop takes these options, after the last public method call.
        let child = self
            .child
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap();
        let sandbox = self.sandbox.take().unwrap();
        // Keep a second owner until spawn succeeds: Builder::spawn drops its
        // closure on failure, which must not delete a live child's TempDir.
        let retained = Arc::new(Mutex::new(Some((child, sandbox))));
        let task = Arc::clone(&retained);
        let spawned = std::thread::Builder::new()
            .name("vst-audio-child-reaper".into())
            .spawn(move || {
                let (mut child, _sandbox) = task
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .unwrap();
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Err(error) if error.raw_os_error() == Some(libc::ECHILD) => break,
                        Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                        Err(_) => {
                            // An error is not proof of exit. Retain ownership and
                            // retry the non-reaping ownership check off the drop thread.
                            std::thread::sleep(Duration::from_secs(1));
                        }
                    }
                }
                // The sandbox is released only after wait confirms exit/reaping.
            });
        if spawned.is_err() {
            // Last-resort resource leak only on thread-spawn failure: preserving
            // the owned child and its directory is safer than premature cleanup.
            std::mem::forget(retained);
            REAPER_SPAWN_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Best-effort scheduling hook. No elevated scheduling promise on Unix.
#[deprecated(note = "use RealtimeAudio")]
#[derive(Debug)]
pub struct Mmcss;
#[allow(deprecated)]
impl Mmcss {
    pub fn pro_audio() -> io::Result<Self> {
        Ok(Self)
    }
}

/// Cadence of the realtime processing loop a [`RealtimeAudio`] guard serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioPeriod {
    pub frames: u32,
    pub sample_rate: u32,
}

/// SCHED_FIFO priority requested for worker audio threads: low, so device and
/// application callback threads (and threaded IRQs) keep precedence, while every
/// SCHED_OTHER thread yields to it.
#[cfg(target_os = "linux")]
pub const FIFO_PRIORITY: i32 = 10;

/// Realtime scheduling for the calling thread, restored when dropped.
/// Not `Send`: it must be dropped on the thread that created it.
///
/// Linux applies `SCHED_FIFO` at `FIFO_PRIORITY` (lowered to a smaller
/// `RLIMIT_RTPRIO`) with `SCHED_RESET_ON_FORK`. A thread that already runs under
/// `SCHED_FIFO` or `SCHED_RR` at that priority or higher, typically inherited from
/// an application started under `chrt`, keeps its scheduling untouched. Dropping
/// restores the previous policy and priority; without `CAP_SYS_NICE` the kernel does not let a thread
/// clear `SCHED_RESET_ON_FORK`, so that flag can remain on the restored policy.
/// macOS applies `THREAD_TIME_CONSTRAINT_POLICY` derived from the
/// [`AudioPeriod`]. Other Unix targets are unsupported. `Debug` shows the applied
/// values.
pub struct RealtimeAudio {
    /// Raw `sched_getscheduler` result before promotion, flags included.
    #[cfg(target_os = "linux")]
    previous_policy: libc::c_int,
    #[cfg(target_os = "linux")]
    previous_priority: libc::c_int,
    /// Applied SCHED_FIFO priority, or the kept inherited one.
    #[cfg(target_os = "linux")]
    priority: libc::c_int,
    /// The thread was already realtime at `priority`; nothing was changed.
    #[cfg(target_os = "linux")]
    inherited: bool,
    /// The calling thread's port name, owned by libpthread (no right of ours).
    #[cfg(target_os = "macos")]
    port: libc::thread_t,
    /// Applied policy, in Mach absolute-time units.
    #[cfg(target_os = "macos")]
    period: u32,
    #[cfg(target_os = "macos")]
    computation: u32,
    #[cfg(target_os = "macos")]
    constraint: u32,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl std::fmt::Debug for RealtimeAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("RealtimeAudio");
        #[cfg(target_os = "linux")]
        debug
            .field(
                "policy",
                &if self.inherited {
                    "inherited realtime"
                } else {
                    "SCHED_FIFO | SCHED_RESET_ON_FORK"
                },
            )
            .field("priority", &self.priority)
            .field("previous_policy", &self.previous_policy)
            .field("previous_priority", &self.previous_priority);
        #[cfg(target_os = "macos")]
        debug
            .field("policy", &"THREAD_TIME_CONSTRAINT_POLICY")
            .field("port", &self.port)
            .field("period_ticks", &self.period)
            .field("computation_ticks", &self.computation)
            .field("constraint_ticks", &self.constraint);
        debug.finish_non_exhaustive()
    }
}

fn validate_period(period: AudioPeriod) -> io::Result<()> {
    if period.frames == 0 || period.sample_rate == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "audio period needs non-zero frames and sample rate",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn rtprio_soft_limit() -> io::Result<libc::rlim_t> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limit points to writable initialized storage for one rlimit.
    if unsafe { libc::getrlimit(libc::RLIMIT_RTPRIO, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(limit.rlim_cur)
}

/// [`FIFO_PRIORITY`], lowered to a finite non-zero soft `RLIMIT_RTPRIO` and kept in
/// the kernel's SCHED_FIFO range. A zero limit keeps the default: the request can
/// still succeed under CAP_SYS_NICE.
#[cfg(target_os = "linux")]
fn fifo_priority(limit: libc::rlim_t) -> io::Result<libc::c_int> {
    let wanted = if limit == 0 || limit == libc::RLIM_INFINITY {
        FIFO_PRIORITY
    } else {
        FIFO_PRIORITY.min(libc::c_int::try_from(limit).unwrap_or(libc::c_int::MAX))
    };
    // SAFETY: scalar-only query of a valid policy constant.
    let min = unsafe { libc::sched_get_priority_min(libc::SCHED_FIFO) };
    // SAFETY: scalar-only query of a valid policy constant.
    let max = unsafe { libc::sched_get_priority_max(libc::SCHED_FIFO) };
    if min < 0 || max < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(wanted.max(min).min(max))
}

#[cfg(target_os = "linux")]
impl RealtimeAudio {
    /// Best effort. An error explains why the OS refused; the caller keeps
    /// processing at normal priority.
    ///
    /// Linux ignores the period beyond validating it: SCHED_FIFO has no notion of
    /// cadence, only priority.
    pub fn promote(period: AudioPeriod) -> io::Result<Self> {
        validate_period(period)?;
        // SAFETY: scalar-only query. On Linux pid 0 names the calling thread (the
        // kernel schedules threads, not processes), so this reads this thread only.
        let previous_policy = unsafe { libc::sched_getscheduler(0) };
        if previous_policy < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut previous = libc::sched_param { sched_priority: 0 };
        // SAFETY: previous is writable initialized storage for one sched_param; pid
        // 0 names the calling thread on Linux.
        if unsafe { libc::sched_getparam(0, &mut previous) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let limit = rtprio_soft_limit()?;
        let priority = fifo_priority(limit)?;
        let current = previous_policy & !libc::SCHED_RESET_ON_FORK;
        if (current == libc::SCHED_FIFO || current == libc::SCHED_RR)
            && previous.sched_priority >= priority
        {
            // Lowering an inherited realtime priority would put the audio thread
            // below the worker's other threads; keep it as it is.
            return Ok(Self {
                previous_policy,
                previous_priority: previous.sched_priority,
                priority: previous.sched_priority,
                inherited: true,
                _thread: std::marker::PhantomData,
            });
        }
        let param = libc::sched_param {
            sched_priority: priority,
        };
        // SAFETY: param is initialized and outlives the call; pid 0 names the
        // calling thread on Linux, so only this thread's policy changes.
        // SCHED_RESET_ON_FORK keeps processes a plugin forks from inheriting it.
        let result = unsafe {
            libc::sched_setscheduler(0, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param)
        };
        if result != 0 {
            let cause = io::Error::last_os_error();
            let limit = if limit == libc::RLIM_INFINITY {
                "unlimited".to_owned()
            } else {
                limit.to_string()
            };
            return Err(io::Error::new(
                cause.kind(),
                format!(
                    "SCHED_FIFO priority {priority} refused ({cause}); RLIMIT_RTPRIO soft \
                     limit {limit}; raise RLIMIT_RTPRIO or grant CAP_SYS_NICE"
                ),
            ));
        }
        Ok(Self {
            previous_policy,
            previous_priority: previous.sched_priority,
            priority,
            inherited: false,
            _thread: std::marker::PhantomData,
        })
    }
}

#[cfg(target_os = "linux")]
impl Drop for RealtimeAudio {
    fn drop(&mut self) {
        if self.inherited {
            return;
        }
        let param = libc::sched_param {
            sched_priority: self.previous_priority,
        };
        // SAFETY: param is initialized and outlives the call. The guard is not
        // Send, so pid 0 still names the thread that promote changed.
        let restored = unsafe { libc::sched_setscheduler(0, self.previous_policy, &param) } == 0;
        if !restored && (self.previous_policy & libc::SCHED_RESET_ON_FORK) == 0 {
            // Without CAP_SYS_NICE the kernel refuses to clear SCHED_RESET_ON_FORK
            // (user_check_sched_setscheduler; sched(7)), so a thread promoted through
            // RLIMIT_RTPRIO alone keeps the flag but still leaves SCHED_FIFO.
            // SAFETY: as above; only the requested flags differ.
            let _ = unsafe {
                libc::sched_setscheduler(
                    0,
                    self.previous_policy | libc::SCHED_RESET_ON_FORK,
                    &param,
                )
            };
        }
    }
}

// XNU bounds for THREAD_TIME_CONSTRAINT_POLICY, verified against
// apple-oss-distributions/xnu main (fetched 2026-11-19):
// - osfmk/kern/thread_policy.c, thread_policy_set_internal (~line 408): rejects
//   `constraint < computation`, `computation > max_rt_quantum` and
//   `computation < min_rt_quantum` with KERN_INVALID_ARGUMENT; it then raises a
//   computation below constraint / 2 to constraint / 2, capped at max_rt_quantum.
//   No other bound applies to period or constraint.
// - osfmk/kern/sched_rt.c, sched_realtime_timebase_init (~lines 102-112):
//   min_rt_quantum = 50 us and max_rt_quantum = 50 ms, converted to absolute time
//   by clock_interval_to_absolutetime_interval.
// - osfmk/kern/sched.h (~line 295): RT_CONSTRAINT_NONE is UINT32_MAX, the run
//   queue's "no constraint" marker, so a real constraint stays below it.
#[cfg(target_os = "macos")]
const MIN_RT_QUANTUM_NS: u64 = 50_000;
#[cfg(target_os = "macos")]
const MAX_RT_QUANTUM_NS: u64 = 50_000_000;
#[cfg(target_os = "macos")]
const MAX_RT_CONSTRAINT: u32 = u32::MAX - 1;

/// Mach absolute time runs at `numer / denom` nanoseconds per tick.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
struct Timebase {
    numer: u128,
    denom: u128,
}

#[cfg(target_os = "macos")]
impl Timebase {
    // libc deprecates its Mach declarations in favour of the mach2 crate; this
    // crate adds no dependency for one call whose ABI is fixed.
    #[allow(deprecated)]
    fn current() -> io::Result<Self> {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: info points to writable initialized storage for one
        // mach_timebase_info; the call only fills it.
        let result = unsafe { libc::mach_timebase_info(&mut info) };
        if result != libc::KERN_SUCCESS || info.numer == 0 || info.denom == 0 {
            return Err(io::Error::other(format!(
                "mach_timebase_info failed (kern_return {result})"
            )));
        }
        Ok(Self {
            numer: info.numer.into(),
            denom: info.denom.into(),
        })
    }

    /// Ticks in `ns` nanoseconds, rounded down.
    fn ticks_floor(self, ns: u128) -> u128 {
        ns * self.denom / self.numer
    }

    /// Ticks in `ns` nanoseconds, rounded up.
    fn ticks_ceil(self, ns: u128) -> u128 {
        (ns * self.denom).div_ceil(self.numer)
    }
}

/// `(period, computation, constraint)` in ticks for one audio period.
///
/// The period and constraint round down, so the deadline is never later than the
/// buffer really lasts; computation is half of that. The quantum bounds round
/// inward (minimum up, maximum down), so the result stays inside the kernel's
/// limits whichever way its own conversion rounds. Periods shorter than the
/// minimum quantum relax the constraint to it, since the kernel rejects a
/// constraint below the computation.
#[cfg(target_os = "macos")]
fn time_constraint(period: AudioPeriod, timebase: Timebase) -> (u32, u32, u32) {
    // u32 frames * 1e9 * u32 denominator stays below 2^94: no u128 overflow.
    let ticks = u128::from(period.frames) * 1_000_000_000 * timebase.denom
        / (u128::from(period.sample_rate) * timebase.numer);
    let min = timebase.ticks_ceil(MIN_RT_QUANTUM_NS.into());
    let max = timebase.ticks_floor(MAX_RT_QUANTUM_NS.into());
    let computation = (ticks / 2).max(min).min(max);
    let constraint = ticks.max(computation).min(u128::from(MAX_RT_CONSTRAINT));
    let saturate = |value: u128| u32::try_from(value).unwrap_or(u32::MAX);
    (saturate(ticks), saturate(computation), saturate(constraint))
}

#[cfg(target_os = "macos")]
impl RealtimeAudio {
    /// Best effort. An error explains why the OS refused; the caller keeps
    /// processing at normal priority.
    ///
    /// The period sets the time-constraint policy: constraint one period,
    /// computation half of it, preemptible, clamped to the kernel's bounds.
    pub fn promote(period: AudioPeriod) -> io::Result<Self> {
        validate_period(period)?;
        let timebase = Timebase::current()?;
        let (ticks, computation, constraint) = time_constraint(period, timebase);
        // SAFETY: no preconditions; returns the calling thread's handle.
        let thread = unsafe { libc::pthread_self() };
        // pthread_mach_thread_np(pthread_self()) returns the port name libpthread
        // cached for this thread (libpthread src/inline_internal.h
        // _pthread_is_valid, ~line 226) and adds no right. mach_thread_self() would
        // copy out a new send right each call (xnu osfmk/kern/ipc_tt.c
        // thread_self_trap, ~line 1294) that needs mach_port_deallocate, which
        // libc does not even declare.
        // SAFETY: thread is the live calling thread, so libpthread knows its port.
        let port = unsafe { libc::pthread_mach_thread_np(thread) };
        let mut policy = libc::thread_time_constraint_policy {
            period: ticks,
            computation,
            constraint,
            preemptible: 1,
        };
        // SAFETY: port names the calling, live thread. policy is initialized,
        // writable and exactly THREAD_TIME_CONSTRAINT_POLICY_COUNT integer_t words;
        // the kernel only reads it during the call.
        let result = unsafe {
            libc::thread_policy_set(
                port,
                libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
                (&raw mut policy).cast::<libc::integer_t>(),
                libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            )
        };
        if result != libc::KERN_SUCCESS {
            let ns = u128::from(period.frames) * 1_000_000_000 / u128::from(period.sample_rate);
            return Err(io::Error::other(format!(
                "THREAD_TIME_CONSTRAINT_POLICY refused (kern_return {result}); period {ns} ns"
            )));
        }
        Ok(Self {
            port,
            period: ticks,
            computation,
            constraint,
            _thread: std::marker::PhantomData,
        })
    }
}

#[cfg(target_os = "macos")]
impl Drop for RealtimeAudio {
    fn drop(&mut self) {
        let mut standard = libc::thread_standard_policy { no_data: 0 };
        // SAFETY: the guard is not Send, so port still names the live calling
        // thread whose right libpthread holds. THREAD_STANDARD_POLICY_COUNT is 0,
        // so the kernel reads nothing through the valid pointer.
        let _ = unsafe {
            libc::thread_policy_set(
                self.port,
                libc::THREAD_STANDARD_POLICY as libc::thread_policy_flavor_t,
                (&raw mut standard).cast::<libc::integer_t>(),
                libc::THREAD_STANDARD_POLICY_COUNT as libc::mach_msg_type_number_t,
            )
        };
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl RealtimeAudio {
    /// Best effort. An error explains why the OS refused; the caller keeps
    /// processing at normal priority. This target has no implementation.
    pub fn promote(period: AudioPeriod) -> io::Result<Self> {
        validate_period(period)?;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "realtime audio scheduling is implemented on Linux and macOS only",
        ))
    }
}

#[cfg(test)]
mod realtime_tests {
    use super::*;

    #[test]
    fn zero_period_is_invalid_input() {
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
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
        }
    }

    #[cfg(target_os = "linux")]
    fn current_policy() -> (libc::c_int, libc::c_int) {
        // SAFETY: scalar-only query; pid 0 names the calling thread on Linux.
        let policy = unsafe { libc::sched_getscheduler(0) };
        assert!(policy >= 0, "{}", io::Error::last_os_error());
        let mut param = libc::sched_param { sched_priority: 0 };
        // SAFETY: param is writable storage for one sched_param; pid 0 names the
        // calling thread on Linux.
        let result = unsafe { libc::sched_getparam(0, &mut param) };
        assert_eq!(result, 0, "{}", io::Error::last_os_error());
        (policy, param.sched_priority)
    }

    /// The documented priority rule, computed independently of `fifo_priority`.
    #[cfg(target_os = "linux")]
    fn expected_priority() -> libc::c_int {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: limit is writable storage for one rlimit.
        let result = unsafe { libc::getrlimit(libc::RLIMIT_RTPRIO, &mut limit) };
        assert_eq!(result, 0, "{}", io::Error::last_os_error());
        let limit = limit.rlim_cur;
        let wanted = if limit == 0 || limit == libc::RLIM_INFINITY {
            FIFO_PRIORITY
        } else {
            limit.min(FIFO_PRIORITY as libc::rlim_t) as libc::c_int
        };
        // SAFETY: scalar-only query.
        let min = unsafe { libc::sched_get_priority_min(libc::SCHED_FIFO) };
        // SAFETY: scalar-only query.
        let max = unsafe { libc::sched_get_priority_max(libc::SCHED_FIFO) };
        wanted.clamp(min, max)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn promote_applies_fifo_or_reports_permission() {
        std::thread::spawn(|| {
            let before = current_policy();
            match RealtimeAudio::promote(AudioPeriod {
                frames: 128,
                sample_rate: 48_000,
            }) {
                Ok(guard) => {
                    let (policy, priority) = current_policy();
                    assert_eq!(policy, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK);
                    assert_eq!(priority, expected_priority());
                    eprintln!("promoted: {guard:?}");
                    drop(guard);
                    // Promotion through RLIMIT_RTPRIO alone (no CAP_SYS_NICE) cannot
                    // clear SCHED_RESET_ON_FORK again; the policy itself must return.
                    let (policy, priority) = current_policy();
                    assert_eq!(
                        (policy & !libc::SCHED_RESET_ON_FORK, priority),
                        (before.0 & !libc::SCHED_RESET_ON_FORK, before.1),
                        "drop did not restore"
                    );
                }
                Err(error) => {
                    eprintln!("refused: {error}");
                    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
                    assert!(error.to_string().contains("RLIMIT_RTPRIO"), "{error}");
                    assert_eq!(current_policy(), before, "refusal changed the policy");
                }
            }
        })
        .join()
        .unwrap();
    }

    /// Clears CAP_SYS_NICE from the calling thread's effective set (capabilities
    /// are per thread on Linux), modelling a thread promoted through
    /// RLIMIT_RTPRIO alone.
    #[cfg(target_os = "linux")]
    fn drop_sys_nice() {
        const VERSION_3: u32 = 0x2008_0522;
        const CAP_SYS_NICE: u32 = 23;
        let mut header = [VERSION_3, 0];
        // Two { effective, permitted, inheritable } words, low then high.
        let mut data = [0u32; 6];
        // SAFETY: header is a valid v3 header naming the calling thread (pid 0) and
        // data is writable storage for the two structures v3 fills.
        let result =
            unsafe { libc::syscall(libc::SYS_capget, header.as_mut_ptr(), data.as_mut_ptr()) };
        assert_eq!(result, 0, "{}", io::Error::last_os_error());
        data[0] &= !(1 << CAP_SYS_NICE);
        // SAFETY: as above; only the calling thread's effective set shrinks.
        let result = unsafe { libc::syscall(libc::SYS_capset, header.as_mut_ptr(), data.as_ptr()) };
        assert_eq!(result, 0, "{}", io::Error::last_os_error());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn drop_restores_policy_without_cap_sys_nice() {
        std::thread::spawn(|| {
            let (before, priority) = current_policy();
            let guard = match RealtimeAudio::promote(AudioPeriod {
                frames: 128,
                sample_rate: 48_000,
            }) {
                Ok(guard) => guard,
                Err(error) => {
                    // Unprivileged CI: promote_applies_fifo_or_reports_permission
                    // covers the refusal.
                    eprintln!("skipped, promotion refused: {error}");
                    return;
                }
            };
            drop_sys_nice();
            drop(guard);
            let (policy, restored) = current_policy();
            assert_eq!(policy & !libc::SCHED_RESET_ON_FORK, before);
            assert_eq!(restored, priority);
        })
        .join()
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inherited_higher_realtime_priority_is_kept() {
        std::thread::spawn(|| {
            let inherited = libc::sched_param {
                sched_priority: FIFO_PRIORITY + 10,
            };
            // SAFETY: inherited is initialized and outlives the call; pid 0 names
            // this test thread only.
            if unsafe { libc::sched_setscheduler(0, libc::SCHED_RR, &inherited) } != 0 {
                eprintln!(
                    "skipped, cannot set up SCHED_RR: {}",
                    io::Error::last_os_error()
                );
                return;
            }
            let guard = RealtimeAudio::promote(AudioPeriod {
                frames: 128,
                sample_rate: 48_000,
            })
            .unwrap();
            assert_eq!(current_policy(), (libc::SCHED_RR, FIFO_PRIORITY + 10));
            drop(guard);
            assert_eq!(current_policy(), (libc::SCHED_RR, FIFO_PRIORITY + 10));
        })
        .join()
        .unwrap();
    }

    #[cfg(target_os = "macos")]
    fn time_constraint_now() -> (libc::thread_time_constraint_policy, libc::boolean_t) {
        let mut policy = libc::thread_time_constraint_policy {
            period: 0,
            computation: 0,
            constraint: 0,
            preemptible: 0,
        };
        let mut count = libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT;
        let mut get_default: libc::boolean_t = 0;
        // SAFETY: no preconditions; returns the calling thread's handle.
        let thread = unsafe { libc::pthread_self() };
        // SAFETY: thread is the live calling thread.
        let port = unsafe { libc::pthread_mach_thread_np(thread) };
        // SAFETY: port names the live calling thread; policy, count and
        // get_default are writable and count states the policy's size in words.
        let result = unsafe {
            libc::thread_policy_get(
                port,
                libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
                (&raw mut policy).cast::<libc::integer_t>(),
                &mut count,
                &mut get_default,
            )
        };
        assert_eq!(result, libc::KERN_SUCCESS);
        (policy, get_default)
    }

    #[cfg(target_os = "macos")]
    fn assert_applied(frames: u32, sample_rate: u32) -> libc::thread_time_constraint_policy {
        let guard = RealtimeAudio::promote(AudioPeriod {
            frames,
            sample_rate,
        })
        .unwrap_or_else(|error| panic!("{frames} frames at {sample_rate} Hz: {error}"));
        let (policy, get_default) = time_constraint_now();
        assert_eq!(
            get_default, 0,
            "{frames} frames at {sample_rate} Hz: {guard:?}"
        );
        let timebase = Timebase::current().unwrap();
        let min = timebase.ticks_ceil(MIN_RT_QUANTUM_NS.into());
        let max = timebase.ticks_floor(MAX_RT_QUANTUM_NS.into());
        let computation = u128::from(policy.computation);
        assert!(
            (min..=max).contains(&computation),
            "computation {computation} outside [{min}, {max}]: {guard:?}"
        );
        assert!(policy.constraint >= policy.computation, "{guard:?}");
        assert_ne!(policy.preemptible, 0);
        drop(guard);
        let (_, get_default) = time_constraint_now();
        assert_ne!(get_default, 0, "drop did not restore the standard policy");
        policy
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn promote_sets_time_constraint_and_drop_restores() {
        std::thread::spawn(|| {
            let policy = assert_applied(128, 48_000);
            let timebase = Timebase::current().unwrap();
            let ns = u128::from(policy.period) * timebase.numer / timebase.denom;
            // One tick of rounding either way, plus integer truncation.
            let tick = timebase.numer.div_ceil(timebase.denom) + 1;
            let expected = 2_666_666u128;
            assert!(
                ns.abs_diff(expected) <= tick,
                "period {ns} ns, expected about {expected} ns"
            );
            assert_eq!(policy.constraint, policy.period);
        })
        .join()
        .unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn long_and_short_periods_are_accepted() {
        std::thread::spawn(|| {
            // About 186 ms: computation must be capped at the maximum quantum.
            assert_applied(8192, 44_100);
            // About 83 us: computation must be raised to the minimum quantum.
            assert_applied(16, 192_000);
        })
        .join()
        .unwrap();
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
    fn drop_bounds_wait_and_reaps_before_removing_sandbox() {
        let sandbox = crate::sandbox::SandboxLaunch::new(
            Some(&crate::sandbox::SandboxPolicy::default()),
            Path::new("/bin/sleep"),
        )
        .unwrap();
        let temporary = sandbox.temporary().unwrap().to_owned();
        let mut child = spawn_handles(
            Path::new("/bin/sleep"),
            &[OsString::from("1")],
            &[],
            parent_handle().unwrap(),
            sandbox,
        )
        .unwrap();
        // TD-43: model a worker that remains alive after termination is requested.
        // The hook belongs to this child only; parallel tests still terminate normally.
        child.suppress_termination = true;
        let pid = child.pid() as libc::pid_t;
        let start = Instant::now();
        drop(child);
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "Drop blocked for {elapsed:?}"
        );
        assert!(temporary.is_dir(), "sandbox removed before child exit");
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: pid identifies this test's child and info is writable siginfo_t
        // storage. WNOWAIT observes exit without stealing the reaper's ownership.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        assert_eq!(result, 0, "child was already reaped");
        // SAFETY: waitid succeeded and initialized the zeroed siginfo_t storage.
        let info = unsafe { info.assume_init() };
        // SAFETY: successful waitid supplies the SIGCHLD layout accessed by si_pid.
        assert_eq!(unsafe { info.si_pid() }, 0, "child already exited");
        let deadline = Instant::now() + Duration::from_secs(5);
        while temporary.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !temporary.exists(),
            "reaper did not remove sandbox after exit"
        );
        let mut status = 0;
        // SAFETY: status is writable and waitpid validates this test's child PID.
        // Only check after cleanup, so this probe cannot reap instead of the reaper.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        assert_eq!(result, -1, "child was not reaped before sandbox cleanup");
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        eprintln!("Drop returned in {elapsed:?}; child reaped and sandbox removed");
    }

    #[test]
    fn stop_inheritance_keeps_the_descriptor_open() {
        let (read, _write) = pipe().unwrap();
        let value = read.as_raw_fd();
        // SAFETY: flag-only change on this test's live descriptor.
        assert_eq!(unsafe { libc::fcntl(value, libc::F_SETFD, 0) }, 0);
        stop_inheritance(value as usize).unwrap();
        // SAFETY: flag-only query on the same live descriptor.
        let flags = unsafe { libc::fcntl(value, libc::F_GETFD) };
        assert_eq!(flags & libc::FD_CLOEXEC, libc::FD_CLOEXEC);
        let error = stop_inheritance(i32::MAX as usize).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EBADF));
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
        // The supervisor calls the legacy wait; an OS error must not panic it.
        let legacy = child.wait(Duration::ZERO);
        // Drop's non-reaping probe returns ECHILD before signalling, and its
        // bounded wait also returns ECHILD. Never signal an already-reaped PID.
        drop(child);
        let error = result.expect_err("external reap must report ECHILD, not a timeout");
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
        assert!(
            elapsed < Duration::from_secs(1),
            "error was delayed: {elapsed:?}"
        );
        assert!(!legacy, "legacy wait reports an unconfirmed exit as false");
    }

    #[test]
    fn fallback_excludes_descriptor_opened_before_limit_was_lowered() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime::tests::lowered_limit_spawn_subprocess",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "isolated descriptor regression failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
    }

    #[test]
    #[ignore = "rlimit mutation runs only in an isolated subprocess"]
    fn lowered_limit_spawn_subprocess() {
        use std::io::Read;

        let file = File::open("/dev/null").unwrap();
        // SAFETY: file is an owned open fd; F_DUPFD returns a new inheritable fd.
        let high = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 256) };
        assert!(
            high >= 256,
            "could not create high fd: {}",
            io::Error::last_os_error()
        );
        // SAFETY: successful F_DUPFD returned unique ownership of high.
        let high = unsafe { OwnedFd::from_raw_fd(high) };
        let limit = libc::rlimit {
            rlim_cur: 64,
            rlim_max: 128,
        };
        // SAFETY: initialized readable rlimit; only this isolated test process is affected.
        let result = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) };
        assert_eq!(result, 0, "could not lower descriptor limit");
        for force_fallback in [true, false] {
            let mut selected = Pipe::create(true).unwrap();
            let script = format!(
                "if test -e /proc/self/fd/{}; then exit 73; fi; printf selected-ok > /proc/self/fd/{}",
                high.as_raw_fd(),
                selected.child_value()
            );
            let sandbox = crate::sandbox::SandboxLaunch::new(None, Path::new("/bin/sh")).unwrap();
            let child = spawn_handles_unix(
                Path::new("/bin/sh"),
                &[OsString::from("-c"), OsString::from(script)],
                &[selected.child_value() as u64],
                parent_handle().unwrap(),
                sandbox,
                force_fallback,
            )
            .expect("descriptor filtering must spawn successfully");
            assert!(child.wait(Duration::from_secs(10)), "probe did not exit");
            assert_eq!(
                child.exit_code().unwrap(),
                Some(0),
                "high unselected descriptor leaked (force_fallback={force_fallback})"
            );
            drop(selected.child);
            let mut received = [0; 11];
            selected.parent.read_exact(&mut received).unwrap();
            assert_eq!(&received, b"selected-ok", "selected fd was not usable");
        }
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
