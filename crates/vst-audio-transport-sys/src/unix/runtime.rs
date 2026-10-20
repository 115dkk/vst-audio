#![allow(unsafe_code)]
//! Selected descriptor inheritance. No plugin code runs here.
use std::{
    ffi::OsString,
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::process::{CommandExt, ExitStatusExt},
    path::Path,
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
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

pub(crate) fn pipe(nonblocking: bool) -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: writable array of two descriptors.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe returned two distinct newly owned descriptors.
    let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: second descriptor is independent of the read end.
    let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    for fd in [&read, &write] {
        // SAFETY: fd is live; setting descriptor flags has no pointer payload.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if nonblocking {
            // SAFETY: pipe descriptor is owned, setting O_NONBLOCK is valid.
            if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok((read, write))
}

#[derive(Debug)]
pub struct ParentHandle {
    read: OwnedFd,
    _write: OwnedFd,
}

pub fn parent_handle() -> io::Result<ParentHandle> {
    let (read, write) = pipe(false)?;
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
        let (read, write) = pipe(false)?;
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

#[derive(Debug)]
pub struct ChildProcess {
    child: Mutex<Child>,
    // Only the parent retains the write end: death closes it even while a
    // plugin's main thread is blocked inside foreign code.
    _parent: ParentHandle,
}

pub fn spawn_handles(
    exe: &Path,
    args: &[OsString],
    values: &[usize],
    parent: ParentHandle,
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
        for fd in &descriptors {
            // SAFETY: selected inherited descriptor, no allocation or pointer access.
            if unsafe { libc::fcntl(*fd, libc::F_SETFD, 0) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    };
    // SAFETY: prepare uses only async-signal-safe syscalls and preallocated data.
    unsafe { command.pre_exec(prepare) };
    let child = command.spawn()?;
    Ok(ChildProcess {
        child: Mutex::new(child),
        _parent: parent,
    })
}

impl ChildProcess {
    pub fn pid(&self) -> u32 {
        self.child.lock().unwrap_or_else(|e| e.into_inner()).id()
    }
    pub fn exit_code(&self) -> io::Result<Option<u32>> {
        Ok(self
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_wait()?
            .map(|s| s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)) as u32))
    }
    pub fn wait(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        loop {
            if self.exit_code().ok().flatten().is_some() {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    pub fn terminate(&self, _: u32) -> io::Result<()> {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        // SAFETY: spawn put this owned, unreaped child in its own process group.
        // Holding the lock prevents another thread reaping/reusing its PID.
        let result = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.terminate(0);
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
