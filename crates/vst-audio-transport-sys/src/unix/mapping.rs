#![allow(unsafe_code)]
//! Anonymous shared file mappings, with a once-fixed disjoint atomic layout.
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
        let file = tempfile::tempfile()?;
        file.set_len(bytes as u64)?;
        Self::map(file, bytes)
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
        if !(640..vst_audio_ring::MAX_REGION_BYTES).contains(&bytes)
            || file.metadata()?.len() != bytes as u64
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid mapping length",
            ));
        }
        // SAFETY: private anonymous backing file, fixed validated size. No safe
        // API exposes truncation or non-atomic views. Its descriptor stays owned.
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
        let word = unsafe { &*((self.address + 32) as *const AtomicU32) };
        u32::from_le(word.load(Ordering::Acquire))
    }
    pub fn region_for(
        &self,
        geometry: Geometry,
    ) -> Result<Region<'_>, vst_audio_ring::GeometryError> {
        let total = geometry.total_bytes()?;
        if total > self.bytes {
            return Err(vst_audio_ring::GeometryError::RegionTooLarge);
        }
        let prefix = total - 128;
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
        let words64 =
            unsafe { std::slice::from_raw_parts((self.address + prefix) as *const AtomicU64, 16) };
        Region::new(words32, words64)
    }
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
