#![allow(unsafe_code)]
//! Anonymous shared file mappings, with a once-fixed disjoint atomic layout.
//!
//! The application owns the backing file and the worker receives a writable
//! descriptor to it. On Linux the file is a `memfd` sealed against shrinking
//! and growing, so a compromised worker cannot remove pages under the
//! application's view (which would fault it with `SIGBUS`). macOS has no file
//! seals; there the backing file is a private temporary file and that
//! truncation remains possible. See `docs/support.md`.
use std::{
    fs::File,
    io,
    os::fd::AsRawFd,
    sync::{
        OnceLock,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
};
use vst_audio_ring::{Geometry, Region};

static MAPPING_ROLE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[derive(Debug)]
pub struct SharedRegion {
    file: File,
    address: usize,
    bytes: usize,
    split: OnceLock<usize>,
}
impl SharedRegion {
    pub fn create(bytes: usize) -> io::Result<Self> {
        if MAPPING_ROLE
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |r| {
                (r != 2).then_some(1)
            })
            .is_err()
        {
            return Err(io::Error::other(
                "worker cannot create application mappings",
            ));
        }
        if !(vst_audio_ring::MIN_REGION_BYTES..vst_audio_ring::MAX_REGION_BYTES).contains(&bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid mapping length",
            ));
        }
        Self::map(backing_file(bytes)?, bytes)
    }
    pub fn open(value: usize, bytes: usize) -> io::Result<Self> {
        if MAPPING_ROLE
            .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(io::Error::other("inherited mapping already opened"));
        }
        Self::map(crate::runtime::inherited_file(value)?, bytes)
    }
    fn map(file: File, bytes: usize) -> io::Result<Self> {
        if !(vst_audio_ring::MIN_REGION_BYTES..vst_audio_ring::MAX_REGION_BYTES).contains(&bytes)
            || file.metadata()?.len() != bytes as u64
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid mapping length",
            ));
        }
        // SAFETY: private anonymous backing file whose length was just validated.
        // No safe API exposes truncation or non-atomic views, and its descriptor
        // stays owned. On Linux the file is sealed, so no other holder of the
        // descriptor can shrink it either; see `backing_file`.
        let pointer = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if pointer == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            file,
            address: pointer as usize,
            bytes,
            split: OnceLock::new(),
        })
    }
    pub fn handle_value(&self) -> usize {
        self.file.as_raw_fd() as usize
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn latency_bound(&self) -> u32 {
        // SAFETY: page-aligned live mapping, word eight is permanently AtomicU32.
        let word = unsafe {
            &*((self.address + vst_audio_ring::LATENCY_BOUND_OFFSET) as *const AtomicU32)
        };
        u32::from_le(word.load(Ordering::Acquire))
    }
    pub fn region_for(
        &self,
        geometry: Geometry,
    ) -> Result<Region<'_>, vst_audio_ring::GeometryError> {
        let prefix = geometry.mapping_split(self.bytes)?;
        if *self.split.get_or_init(|| prefix) != prefix {
            return Err(vst_audio_ring::GeometryError::RegionTooLarge);
        }
        Ok(self.region())
    }
    pub fn region(&self) -> Region<'_> {
        let prefix = *self.split.get().expect("region_for must fix geometry");
        // SAFETY: validated page-aligned prefix accessed only as atomic u32 words;
        // view lifetime is borrowed from the mapping.
        let words32 =
            unsafe { std::slice::from_raw_parts(self.address as *const AtomicU32, prefix / 4) };
        // SAFETY: disjoint aligned fixed 128-byte suffix, only atomic u64 accesses.
        let words64 = unsafe {
            std::slice::from_raw_parts(
                (self.address + prefix) as *const AtomicU64,
                vst_audio_ring::COUNTER_WORDS,
            )
        };
        Region::new(words32, words64)
    }
}
/// Creates the file behind a new application mapping, already at its final length.
fn backing_file(bytes: usize) -> io::Result<File> {
    #[cfg(target_os = "linux")]
    match sealed_memfd(bytes) {
        // Only a kernel without memfd_create falls back to an unsealed file.
        Err(error) if error.raw_os_error() == Some(libc::ENOSYS) => {}
        result => return result,
    }
    let file = tempfile::tempfile()?;
    file.set_len(bytes as u64)?;
    Ok(file)
}

/// An anonymous memory file that no descriptor holder can shrink or grow.
#[cfg(target_os = "linux")]
fn sealed_memfd(bytes: usize) -> io::Result<File> {
    use std::os::fd::{FromRawFd, OwnedFd};
    // SAFETY: constant NUL-terminated name; the flags are documented memfd flags.
    let fd = unsafe {
        libc::memfd_create(
            c"vst-audio-region".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: memfd_create returned a new descriptor that nothing else owns.
    let file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    file.set_len(bytes as u64)?;
    let seals = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
    // SAFETY: owned descriptor; adding seals carries no pointer payload.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

impl vst_audio_ring::RegionMemory for SharedRegion {
    fn region(&self) -> Region<'_> {
        self.region()
    }
}
impl Drop for SharedRegion {
    fn drop(&mut self) {
        // SAFETY: uniquely owned mapping; borrowed views cannot outlive self.
        unsafe { libc::munmap(self.address as *mut _, self.bytes) };
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn worker_descriptor_cannot_resize_the_application_mapping() {
        let bytes = 64 * 1024;
        let region = SharedRegion::create(bytes).unwrap();
        // The worker receives exactly this descriptor value and duplicates it.
        let worker = crate::runtime::inherited_file(region.handle_value()).unwrap();
        for length in [0, bytes as u64 / 2, bytes as u64 * 2] {
            let error = worker.set_len(length).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{length}");
        }
        assert_eq!(worker.metadata().unwrap().len(), bytes as u64);
        // The sealed file still backs a working mapping of the validated length.
        assert_eq!(region.bytes(), bytes);
        assert_eq!(region.latency_bound(), 0);
    }
}
