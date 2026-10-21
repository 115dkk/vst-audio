#![allow(unsafe_code, non_snake_case)]
//! Host-owned COM objects. Controller callbacks are UI-thread-only (SDK threading model).
use crate::{editor::EditorWindow, state_buffer::StateBuffer};
use std::{
    cell::RefCell,
    ffi::c_void,
    io::{Read, Seek, SeekFrom, Write},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
};
use vst3::{Class, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*};

pub(crate) const CAPACITY: usize = 256;
pub(crate) struct Edits {
    slots: [(AtomicU32, AtomicU64); CAPACITY],
    read: AtomicUsize,
    write: AtomicUsize,
    pub flags: AtomicU32,
}
impl Edits {
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| (AtomicU32::new(0), AtomicU64::new(0))),
            read: AtomicUsize::new(0),
            write: AtomicUsize::new(0),
            flags: AtomicU32::new(0),
        }
    }
    pub(crate) fn push(&self, id: u32, value: f64) -> bool {
        let write = self.write.load(Ordering::Relaxed);
        if write.wrapping_sub(self.read.load(Ordering::Acquire)) == CAPACITY {
            return false;
        }
        let slot = &self.slots[write % CAPACITY];
        slot.0.store(id, Ordering::Relaxed);
        slot.1.store(value.to_bits(), Ordering::Relaxed);
        self.write.store(write.wrapping_add(1), Ordering::Release);
        true
    }
    pub fn pop(&self) -> Option<(u32, f64)> {
        let read = self.read.load(Ordering::Relaxed);
        if read == self.write.load(Ordering::Acquire) {
            return None;
        }
        let slot = &self.slots[read % CAPACITY];
        let edit = (
            slot.0.load(Ordering::Relaxed),
            f64::from_bits(slot.1.load(Ordering::Relaxed)),
        );
        self.read.store(read.wrapping_add(1), Ordering::Release);
        Some(edit)
    }
}
pub(crate) struct Host {
    #[cfg(target_os = "linux")]
    pub run_loop: Rc<crate::linux_runloop::RunLoop>,
}
impl Host {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            run_loop: Rc::new(crate::linux_runloop::RunLoop::default()),
        }
    }
}
#[cfg(target_os = "linux")]
crate::linux_runloop::implement!(Host, run_loop);
impl Class for Host {
    #[cfg(not(target_os = "linux"))]
    type Interfaces = (IHostApplication,);
    #[cfg(target_os = "linux")]
    type Interfaces = (IHostApplication, Linux::IRunLoop);
}
impl IHostApplicationTrait for Host {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: SDK supplies one writable String128 for this synchronous call.
        let name = unsafe { &mut *name };
        name.fill(0);
        for (dst, src) in name.iter_mut().zip("vst-audio".encode_utf16()) {
            *dst = src;
        }
        kResultOk
    }
    unsafe fn createInstance(
        &self,
        _: *mut TUID,
        _: *mut TUID,
        object: *mut *mut c_void,
    ) -> tresult {
        if !object.is_null() {
            // SAFETY: optional SDK output pointer; no object is created.
            unsafe { *object = std::ptr::null_mut() };
        }
        kNotImplemented
    }
}
pub(crate) struct Handler(pub Arc<Edits>);
impl Class for Handler {
    type Interfaces = (IComponentHandler,);
}
impl IComponentHandlerTrait for Handler {
    unsafe fn beginEdit(&self, _: u32) -> tresult {
        self.0.flags.fetch_or(1, Ordering::Release);
        kResultOk
    }
    unsafe fn performEdit(&self, id: u32, value: f64) -> tresult {
        self.0.flags.fetch_or(1, Ordering::Release);
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return kInvalidArgument;
        }
        if self.0.push(id, value) {
            kResultOk
        } else {
            // Never silently drop automation: report a fault if the bounded queue is exhausted.
            self.0.flags.fetch_or(8, Ordering::Release);
            kResultFalse
        }
    }
    unsafe fn endEdit(&self, _: u32) -> tresult {
        self.0.flags.fetch_or(1, Ordering::Release);
        kResultOk
    }
    unsafe fn restartComponent(&self, flags: i32) -> tresult {
        if flags & RestartFlags_::kLatencyChanged != 0 {
            self.0.flags.fetch_or(2, Ordering::Release);
        }
        if flags & RestartFlags_::kIoChanged != 0 {
            self.0.flags.fetch_or(4, Ordering::Release);
        }
        kResultOk
    }
}

pub(crate) struct Stream(pub RefCell<StateBuffer>);
impl Stream {
    pub fn new(bytes: Vec<u8>) -> ComWrapper<Self> {
        ComWrapper::new(Self(RefCell::new(StateBuffer::new(bytes))))
    }
}
impl Class for Stream {
    type Interfaces = (IBStream,);
}
impl IBStreamTrait for Stream {
    unsafe fn read(&self, buffer: *mut c_void, count: i32, actual: *mut i32) -> tresult {
        if count < 0 || (count > 0 && buffer.is_null()) {
            return kInvalidArgument;
        }
        let mut empty = [];
        let bytes = if count == 0 {
            &mut empty[..]
        } else {
            // SAFETY: IBStream caller provides count writable bytes; used only in this call.
            unsafe { std::slice::from_raw_parts_mut(buffer.cast::<u8>(), count as usize) }
        };
        let Ok(n) = self.0.borrow_mut().read(bytes) else {
            return kResultFalse;
        };
        if !actual.is_null() {
            // SAFETY: optional writable byte-count output supplied by the caller.
            unsafe { *actual = n as i32 };
        }
        if n == count as usize {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn write(&self, buffer: *mut c_void, count: i32, actual: *mut i32) -> tresult {
        if count < 0 || (count > 0 && buffer.is_null()) {
            return kInvalidArgument;
        }
        let bytes = if count == 0 {
            &[][..]
        } else {
            // SAFETY: IBStream caller provides count readable bytes for this synchronous write.
            unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), count as usize) }
        };
        let Ok(n) = self.0.borrow_mut().write(bytes) else {
            return kResultFalse;
        };
        if !actual.is_null() {
            // SAFETY: optional writable byte-count output supplied by the caller.
            unsafe { *actual = n as i32 };
        }
        kResultOk
    }
    unsafe fn seek(&self, position: i64, mode: i32, actual: *mut i64) -> tresult {
        let from = match mode {
            0 if position >= 0 => SeekFrom::Start(position as u64),
            1 => SeekFrom::Current(position),
            2 => SeekFrom::End(position),
            _ => return kInvalidArgument,
        };
        let Ok(position) = self.0.borrow_mut().seek(from) else {
            return kInvalidArgument;
        };
        if !actual.is_null() {
            // SAFETY: optional writable seek-position output supplied by caller.
            unsafe { *actual = position as i64 };
        }
        kResultOk
    }
    unsafe fn tell(&self, position: *mut i64) -> tresult {
        if position.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: non-null writable position output required by IBStream.
        unsafe { *position = self.0.borrow().position() as i64 };
        kResultOk
    }
}

pub(crate) use crate::vst3_events::Changes;

pub(crate) struct Frame {
    pub window: Rc<EditorWindow>,
    #[cfg(target_os = "linux")]
    pub run_loop: Rc<crate::linux_runloop::RunLoop>,
}
#[cfg(target_os = "linux")]
crate::linux_runloop::implement!(Frame, run_loop);
impl Class for Frame {
    #[cfg(not(target_os = "linux"))]
    type Interfaces = (IPlugFrame,);
    #[cfg(target_os = "linux")]
    type Interfaces = (IPlugFrame, Linux::IRunLoop);
}
impl IPlugFrameTrait for Frame {
    unsafe fn resizeView(&self, view: *mut IPlugView, rect: *mut ViewRect) -> tresult {
        if rect.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: SDK resize callback supplies a rectangle valid for this call.
        let size = unsafe { *rect };
        let Some(width) = size.right.checked_sub(size.left) else {
            return kInvalidArgument;
        };
        let Some(height) = size.bottom.checked_sub(size.top) else {
            return kInvalidArgument;
        };
        if !(1..=8192).contains(&width) || !(1..=8192).contains(&height) {
            return kInvalidArgument;
        }
        // SAFETY: callback supplies the requesting live view; borrow does not escape.
        let Some(view) = (unsafe { ComRef::from_raw(view) }) else {
            return kInvalidArgument;
        };
        if self.window.resize(width, height).is_err() {
            return kResultFalse;
        }
        // SAFETY: main-thread callback, rectangle remains valid; window has been resized first.
        unsafe { view.onSize(rect) }
    }
}
