#![allow(unsafe_code)]
use std::ffi::CStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::{
    Win32::{
        Foundation::{E_INVALIDARG, FreeLibrary, HMODULE},
        System::LibraryLoader::{GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW},
    },
    core::PCWSTR,
};

#[derive(Debug)]
pub struct Library(HMODULE);
impl Library {
    pub fn load(path: &Path) -> windows::core::Result<Self> {
        if !path.is_absolute() {
            return Err(windows::core::Error::from_hresult(E_INVALIDARG));
        }
        let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
        if name.contains(&0) {
            return Err(windows::core::Error::from_hresult(E_INVALIDARG));
        }
        name.push(0);
        // SAFETY: terminated absolute path; code is loaded only in the disposable host.
        let module =
            unsafe { LoadLibraryExW(PCWSTR(name.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }
                .inspect_err(|_| {
                if std::env::var_os("VST_AUDIO_TRACE").is_some() {
                    diagnose(path, &name);
                }
            })?;
        Ok(Self(module))
    }
    pub fn symbol(&self, name: &CStr) -> Option<unsafe extern "system" fn() -> isize> {
        // SAFETY: live module and terminated symbol name; caller must validate callable ABI.
        unsafe { GetProcAddress(self.0, windows::core::PCSTR(name.as_ptr().cast())) }
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: uniquely owned module, dropped after all plugin objects and callbacks.
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

/// Temporary: separates file access, image mapping, dependency loading and
/// thread access when a sandboxed load is denied.
fn diagnose(path: &Path, name: &[u16]) {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            LibraryLoader::{DONT_RESOLVE_DLL_REFERENCES, LOAD_LIBRARY_AS_DATAFILE},
            Threading::{
                GetCurrentProcessId, GetThreadId, OpenProcess, OpenThread,
                PROCESS_QUERY_LIMITED_INFORMATION, THREAD_QUERY_LIMITED_INFORMATION,
                THREAD_SET_LIMITED_INFORMATION,
            },
        },
    };
    let read = std::fs::File::open(path).map(|_| ());
    let execute = std::fs::OpenOptions::new()
        .access_mode(0x8000_0000 | 0x2000_0000)
        .open(path)
        .map(|_| ());
    let flags = |flags| {
        // SAFETY: terminated path; the module is released immediately.
        unsafe { LoadLibraryExW(PCWSTR(name.as_ptr()), None, flags) }.map(|module| {
            // SAFETY: module was just loaded by this function.
            let _ = unsafe { FreeLibrary(module) };
        })
    };
    let data = flags(LOAD_LIBRARY_AS_DATAFILE);
    let unresolved = flags(DONT_RESOLVE_DLL_REFERENCES);
    let sleeper = std::thread::spawn(|| std::thread::sleep(std::time::Duration::from_millis(200)));
    // SAFETY: live join handle owned by this function.
    let id = unsafe { GetThreadId(HANDLE(sleeper.as_raw_handle())) };
    // SAFETY: opens by identifier; the handle is closed below.
    let thread = unsafe {
        OpenThread(
            THREAD_QUERY_LIMITED_INFORMATION | THREAD_SET_LIMITED_INFORMATION,
            false,
            id,
        )
    }
    .map(|handle| {
        // SAFETY: handle was just opened.
        let _ = unsafe { CloseHandle(handle) };
    });
    // SAFETY: no arguments; returns this process's identifier.
    let pid = unsafe { GetCurrentProcessId() };
    // SAFETY: opens this process by identifier; the handle is closed below.
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map(|handle| {
            // SAFETY: handle was just opened.
            let _ = unsafe { CloseHandle(handle) };
        });
    let _ = sleeper.join();
    eprintln!(
        "load diagnosis {}: read={read:?} execute={execute:?} datafile={data:?} unresolved={unresolved:?} own_thread={thread:?} own_process={process:?}",
        path.display()
    );
}
