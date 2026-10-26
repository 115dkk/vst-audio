#![allow(unsafe_code)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Arc;
use std::time::Duration;
use vst_audio_ring::{Signal, Wait, Waited};
use windows::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Security::SECURITY_ATTRIBUTES,
    System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects},
};

#[derive(Debug)]
pub struct Event {
    handle: OwnedHandle,
    peer: Option<OwnedHandle>,
}
impl Event {
    pub fn create() -> windows::core::Result<Self> {
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            bInheritHandle: false.into(),
            ..Default::default()
        };
        // SAFETY: valid attributes; unnamed, auto-reset, initially unsignalled event.
        let handle = unsafe { CreateEventW(Some(&attributes), false, false, None) }?;
        // SAFETY: successful creation transfers a unique kernel handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        Ok(Self { handle, peer: None })
    }
    /// Stops a process the plugin starts from inheriting the token's handle.
    pub fn stop_inheritance(value: u64) -> windows::core::Result<()> {
        let value = usize::try_from(value).map_err(|_| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_INVALIDARG)
        })?;
        crate::runtime::stop_inheritance(value)
    }
    pub fn open(value: u64) -> windows::core::Result<Self> {
        let value = usize::try_from(value).map_err(|_| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_INVALIDARG)
        })?;
        Ok(Self {
            handle: crate::runtime::duplicate_value(value, false)?,
            peer: None,
        })
    }
    pub fn with_peer(mut self, peer: OwnedHandle) -> Self {
        self.peer = Some(peer);
        self
    }
    pub fn handle_value(&self) -> u64 {
        self.handle.as_raw_handle() as usize as u64
    }
}
impl Signal for Event {
    fn signal(&self) {
        // SAFETY: the owned event handle stays live throughout this nonblocking call.
        let _ = unsafe { SetEvent(HANDLE(self.handle.as_raw_handle())) };
    }
}
impl Wait for Event {
    fn wait(&self, timeout: Duration) -> Waited {
        let event = HANDLE(self.handle.as_raw_handle());
        let handles = [
            event,
            self.peer
                .as_ref()
                .map_or(event, |peer| HANDLE(peer.as_raw_handle())),
        ];
        let count = if self.peer.is_some() { 2 } else { 1 };
        let milliseconds = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
        // SAFETY: handles are owned for the entire wait; slice has one or two entries.
        let result = unsafe { WaitForMultipleObjects(&handles[..count], false, milliseconds) };
        if result == WAIT_OBJECT_0 {
            Waited::Signalled
        } else if result == WAIT_TIMEOUT {
            Waited::TimedOut
        } else {
            Waited::PeerGone
        }
    }
}

/// A shared [`Event`] that is itself a [`Signal`] and a [`Wait`]. The ring crate
/// already implements `Signal for Arc<T>`, but not `Wait for Arc<T>`; this wrapper
/// retains shared waiting and compatibility with existing callers (audit #11 TD-80).
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
