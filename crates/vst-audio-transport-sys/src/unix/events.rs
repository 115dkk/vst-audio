#![allow(unsafe_code)]
//! Bounded nonblocking wakeups; counts are hints, ring sequences are truth.
//!
//! A wakeup is a one-byte datagram on a connected Unix socket pair, sent and
//! received with a per-call `MSG_DONTWAIT`. The worker shares these open file
//! descriptions, so a flag it could change (`O_NONBLOCK`) must never decide
//! whether the application's realtime thread blocks.
use std::{
    io,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::net::UnixDatagram,
    },
    sync::Arc,
    time::Duration,
};

/// Stale wakeups drained per wait; a flooding peer cannot keep this looping.
const DRAIN_LIMIT: usize = 64;
use vst_audio_ring::{Signal, Wait, Waited};

#[derive(Debug)]
pub struct Event {
    read: OwnedFd,
    write: OwnedFd,
    peer: Option<OwnedFd>,
}
impl Event {
    pub fn create() -> io::Result<Self> {
        let (read, write) = UnixDatagram::pair()?;
        Ok(Self {
            read: read.into(),
            write: write.into(),
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
        // SAFETY: owned connected socket and one readable stack byte. MSG_DONTWAIT
        // keeps the call nonblocking whatever the shared O_NONBLOCK flag says; a
        // full queue (EAGAIN) already represents a pending wakeup.
        unsafe {
            libc::send(
                self.write.as_raw_fd(),
                (&byte as *const u8).cast(),
                1,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
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
            let mut bytes = [0u8; 16];
            for _ in 0..DRAIN_LIMIT {
                // SAFETY: writable stack buffer; MSG_DONTWAIT bounds the call
                // whatever the shared O_NONBLOCK flag says.
                let received = unsafe {
                    libc::recv(
                        self.read.as_raw_fd(),
                        bytes.as_mut_ptr().cast(),
                        bytes.len(),
                        libc::MSG_DONTWAIT,
                    )
                };
                if received <= 0 {
                    break;
                }
            }
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

    #[test]
    fn signal_never_blocks_after_the_worker_clears_nonblocking() {
        let event = Event::create().unwrap();
        // The worker's duplicates share both open file descriptions.
        let opened = Event::open(event.handle_value()).unwrap();
        for fd in [opened.read.as_raw_fd(), opened.write.as_raw_fd()] {
            // SAFETY: owned descriptor; clearing status flags has no pointer payload.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, 0) }, 0);
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..100_000 {
                event.signal();
            }
            sender.send(()).unwrap();
        });
        assert!(
            receiver.recv_timeout(Duration::from_secs(20)).is_ok(),
            "signal blocked once the queue was full"
        );
        assert_eq!(opened.wait(Duration::from_secs(1)), Waited::Signalled);
        // Draining is bounded, so later waits still see the stale wakeups
        // instead of blocking until the timeout.
        assert_eq!(opened.wait(Duration::from_secs(1)), Waited::Signalled);
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
