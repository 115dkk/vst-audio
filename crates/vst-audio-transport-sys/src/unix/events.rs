#![allow(unsafe_code)]
//! Bounded nonblocking pipe wakeups; counts are hints, ring sequences are truth.
use std::{
    io,
    os::fd::{AsRawFd, OwnedFd},
    sync::Arc,
    time::Duration,
};
use vst_audio_ring::{Signal, Wait, Waited};

#[derive(Debug)]
pub struct Event {
    read: OwnedFd,
    write: OwnedFd,
    peer: Option<OwnedFd>,
}
impl Event {
    pub fn create() -> io::Result<Self> {
        let (read, write) = crate::runtime::pipe(true)?;
        Ok(Self {
            read,
            write,
            peer: None,
        })
    }
    pub fn open(value: u64) -> io::Result<Self> {
        Ok(Self {
            read: crate::runtime::duplicate_value((value & 0xffff_ffff) as usize, false)?,
            write: crate::runtime::duplicate_value((value >> 32) as usize, false)?,
            peer: None,
        })
    }
    pub fn with_peer(mut self, peer: OwnedFd) -> Self {
        self.peer = Some(peer);
        self
    }
    pub fn handle_value(&self) -> u64 {
        self.read.as_raw_fd() as u64 | ((self.write.as_raw_fd() as u64) << 32)
    }
}
impl Signal for Event {
    fn signal(&self) {
        let byte = 1u8;
        // SAFETY: live nonblocking pipe descriptor and one readable stack byte.
        // A full pipe already represents a wakeup; EAGAIN is intentionally ignored.
        unsafe { libc::write(self.write.as_raw_fd(), (&byte as *const u8).cast(), 1) };
    }
}
impl Wait for Event {
    fn wait(&self, timeout: Duration) -> Waited {
        let mut fds = [
            libc::pollfd {
                fd: self.read.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.peer.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: two initialized pollfd records; descriptors remain owned.
        let result = unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                2,
                timeout.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if result == 0 {
            return Waited::TimedOut;
        }
        if result < 0 {
            return if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                Waited::TimedOut
            } else {
                Waited::PeerGone
            };
        }
        if fds[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Waited::PeerGone;
        }
        if fds[0].revents & libc::POLLIN != 0 {
            let mut bytes = [0u8; 256];
            // SAFETY: writable stack buffer; read is nonblocking and bounded.
            unsafe {
                libc::read(
                    self.read.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            Waited::Signalled
        } else {
            Waited::PeerGone
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_token_preserves_both_descriptors() {
        let event = Event::create().unwrap();
        let token = event.handle_value();
        assert_eq!(token & 0xffff_ffff, event.read.as_raw_fd() as u64);
        assert_eq!(token >> 32, event.write.as_raw_fd() as u64);
        let opened = Event::open(token).unwrap();
        event.signal();
        assert_eq!(opened.wait(Duration::from_secs(1)), Waited::Signalled);
        opened.signal();
        assert_eq!(event.wait(Duration::from_secs(1)), Waited::Signalled);
        assert!(Event::open(u64::MAX).is_err());
    }
}

#[derive(Clone, Debug)]
pub struct SharedEvent(pub Arc<Event>);
impl Signal for SharedEvent {
    fn signal(&self) {
        self.0.signal();
    }
}
impl Wait for SharedEvent {
    fn wait(&self, timeout: Duration) -> Waited {
        self.0.wait(timeout)
    }
}
