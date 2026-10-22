#![allow(unsafe_code)]
use super::{SandboxPolicy, SandboxReport, seatbelt_profile};
use std::{
    ffi::{CStr, CString},
    io,
    path::Path,
};

// Public libSystem sandbox.h API; profile text uses flags=0, not a named profile.
// https://github.com/chromium/chromium/blob/main/sandbox/mac/seatbelt.cc
unsafe extern "C" {
    fn sandbox_init(
        profile: *const libc::c_char,
        flags: u64,
        error: *mut *mut libc::c_char,
    ) -> libc::c_int;
    fn sandbox_free_error(error: *mut libc::c_char);
}
pub fn apply_worker(policy: &SandboxPolicy, temporary: &Path) -> io::Result<SandboxReport> {
    let profile = CString::new(seatbelt_profile(policy, temporary)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "sandbox profile contains NUL"))?;
    let mut error = std::ptr::null_mut();
    // SAFETY: profile is NUL-terminated and error is writable; no plugin threads exist.
    let result = unsafe { sandbox_init(profile.as_ptr(), 0, &mut error) };
    let diagnostic = if error.is_null() {
        String::from("sandbox_init failed")
    } else {
        // SAFETY: sandbox_init returned a NUL-terminated diagnostic owned by its API.
        let text = unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: the returned diagnostic is freed exactly once with its designated allocator.
        unsafe { sandbox_free_error(error) };
        text
    };
    if result != 0 {
        return Err(io::Error::other(diagnostic));
    }
    Ok(SandboxReport::applied(policy, "seatbelt".into()))
}
