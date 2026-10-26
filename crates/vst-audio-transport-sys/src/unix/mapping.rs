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
        let file = crate::runtime::inherited_file(value)?;
        #[cfg(target_os = "linux")]
        {
            // audit #11 TD-01: an inherited unsealed file can be truncated by its sender;
            // opening_an_unsealed_descriptor_is_refused covers that case.
            // SAFETY: live owned descriptor; F_GET_SEALS has no pointer payload.
            let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
            if seals < 0 {
                return Err(io::Error::last_os_error());
            }
            let required = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW;
            if seals & required != required {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "mapping must be sealed against shrinking and growing",
                ));
            }
        }
        Self::map(file, bytes)
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
    {
        sealed_memfd(bytes)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let file = tempfile::tempfile()?;
        file.set_len(bytes as u64)?;
        Ok(file)
    }
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
    fn opening_an_unsealed_descriptor_is_refused() {
        const CASE_ENV: &str = "VST_AUDIO_MAPPING_SEAL_TEST";
        let Ok(case) = std::env::var(CASE_ENV) else {
            for case in ["unsealed", "tempfile", "shrink-only", "grow-only", "sealed"] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "mapping::tests::opening_an_unsealed_descriptor_is_refused",
                        "--nocapture",
                    ])
                    .env(CASE_ENV, case)
                    .output()
                    .unwrap();
                assert!(
                    String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
                    "child must execute the descriptor test",
                );
                assert!(
                    output.status.success(),
                    "{case}:\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }
            return;
        };
        let bytes = 64 * 1024;
        let file = if case == "tempfile" {
            tempfile::tempfile().unwrap()
        } else {
            use std::os::fd::{FromRawFd, OwnedFd};
            // SAFETY: constant NUL-terminated name and documented memfd flags.
            let fd = unsafe {
                libc::memfd_create(
                    c"vst-audio-unsealed-test".as_ptr(),
                    libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
                )
            };
            assert!(fd >= 0, "{}", io::Error::last_os_error());
            // SAFETY: memfd_create returned a new descriptor with no other owner.
            File::from(unsafe { OwnedFd::from_raw_fd(fd) })
        };
        file.set_len(bytes as u64).unwrap();
        let seals = match case.as_str() {
            "shrink-only" => libc::F_SEAL_SHRINK,
            "grow-only" => libc::F_SEAL_GROW,
            "sealed" => libc::F_SEAL_SHRINK | libc::F_SEAL_GROW,
            _ => 0,
        };
        if seals != 0 {
            // SAFETY: live owned descriptor; adding seals has no pointer payload.
            let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) };
            assert_eq!(result, 0);
        }
        let result = SharedRegion::open(file.as_raw_fd() as usize, bytes);
        if case == "sealed" {
            let region = result.unwrap();
            assert_eq!(region.bytes(), bytes);
            assert_eq!(region.latency_bound(), 0);
        } else {
            assert!(result.is_err(), "{case} descriptor must not be mapped");
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
    }

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
