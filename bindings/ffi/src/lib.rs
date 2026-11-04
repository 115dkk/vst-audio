#![deny(unsafe_code)]
//! C ABI for the safe, out-of-process `vst-audio` host.

mod core;

use crate::core::{Error, HostHandle, Plugin};
use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    ptr,
    time::Duration,
};
use vst_audio::{ClassInfo, Parameter, SampleFormat};

pub const VSTA_OK: i32 = 0;
pub const VSTA_INVALID_ARGUMENT: i32 = 1;
pub const VSTA_START: i32 = 2;
pub const VSTA_RENDER: i32 = 3;
pub const VSTA_REQUEST_TIMEOUT: i32 = 4;
pub const VSTA_FAILED: i32 = 5;
pub const VSTA_PANIC: i32 = 6;

#[derive(Debug)]
struct LastError {
    message: String,
    kind: String,
}

thread_local! {
    static LAST_ERROR: RefCell<LastError> = const { RefCell::new(LastError {
        message: String::new(),
        kind: String::new(),
    }) };
}

#[derive(Debug)]
pub struct VstaHost(HostHandle);

#[derive(Debug)]
pub struct VstaPlugin(Plugin);

#[derive(Debug)]
pub struct VstaClassList(Vec<ClassInfo>);

#[derive(Debug)]
pub struct VstaParameterList(Vec<Parameter>);

#[derive(Debug)]
pub struct VstaBytes(Vec<u8>);

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VstaClassInfo {
    pub id: [u8; 16],
    pub name: *const u8,
    pub name_len: usize,
    pub vendor: *const u8,
    pub vendor_len: usize,
    pub category: *const u8,
    pub category_len: usize,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VstaPluginInfo {
    pub input_channels: u32,
    pub output_channels: u32,
    pub sample_rate: u32,
    pub block_size: u32,
    pub sample_format: i32,
    pub latency: u32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VstaParameter {
    pub id: u32,
    pub name: *const u8,
    pub name_len: usize,
    pub unit: *const u8,
    pub unit_len: usize,
    pub value: f64,
    pub default_value: f64,
    pub has_default: u8,
    pub steps: u32,
    pub has_steps: u8,
    pub automatable: u8,
    pub read_only: u8,
    pub hidden: u8,
}

fn set_last_error(message: impl Into<String>, kind: impl Into<String>) {
    LAST_ERROR.with(|last| {
        *last.borrow_mut() = LastError {
            message: message.into(),
            kind: kind.into(),
        };
    });
}

fn error_status(error: &Error) -> i32 {
    match error {
        Error::InvalidArgument(_) => VSTA_INVALID_ARGUMENT,
        Error::Start(_) => VSTA_START,
        Error::Render(_) => VSTA_RENDER,
        Error::RequestTimeout => VSTA_REQUEST_TIMEOUT,
        Error::Failed(_) => VSTA_FAILED,
    }
}

fn report_error(error: Error) -> i32 {
    let status = error_status(&error);
    set_last_error(error.message(), error.kind());
    status
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic in vst-audio FFI".into()
    }
}

fn report_panic(payload: Box<dyn std::any::Any + Send>) -> i32 {
    set_last_error(panic_message(payload), "panic");
    VSTA_PANIC
}

fn status_call(call: impl FnOnce() -> Result<(), Error>) -> i32 {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(Ok(())) => VSTA_OK,
        Ok(Err(error)) => report_error(error),
        Err(payload) => report_panic(payload),
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::invalid(message)
}

#[allow(unsafe_code)]
unsafe fn shared<'a, T>(pointer: *const T, name: &str) -> Result<&'a T, Error> {
    if pointer.is_null() || !pointer.is_aligned() {
        return Err(invalid(format!("{name} is null or misaligned")));
    }
    // SAFETY: The caller promises that `pointer` addresses a live `T` for this call.
    Ok(unsafe { &*pointer })
}

#[allow(unsafe_code)]
unsafe fn exclusive<'a, T>(pointer: *mut T, name: &str) -> Result<&'a mut T, Error> {
    if pointer.is_null() || !pointer.is_aligned() {
        return Err(invalid(format!("{name} is null or misaligned")));
    }
    // SAFETY: The caller promises exclusive access to a live `T` for this call.
    Ok(unsafe { &mut *pointer })
}

#[allow(unsafe_code)]
unsafe fn input_slice<'a, T>(
    pointer: *const T,
    length: usize,
    name: &str,
) -> Result<&'a [T], Error> {
    // An empty buffer may arrive as null: C# pins an empty array to null.
    if length == 0 {
        return Ok(&[]);
    }
    if pointer.is_null() || !pointer.is_aligned() {
        return Err(invalid(format!("{name} is null or misaligned")));
    }
    if length > isize::MAX as usize / std::mem::size_of::<T>() {
        return Err(invalid(format!("{name} is too long")));
    }
    // SAFETY: The caller promises `length` initialized `T` values at `pointer`.
    Ok(unsafe { std::slice::from_raw_parts(pointer, length) })
}

#[allow(unsafe_code)]
unsafe fn output_slice<'a, T>(
    pointer: *mut T,
    length: usize,
    name: &str,
) -> Result<&'a mut [T], Error> {
    if length == 0 {
        return Ok(&mut []);
    }
    if pointer.is_null() || !pointer.is_aligned() {
        return Err(invalid(format!("{name} is null or misaligned")));
    }
    if length > isize::MAX as usize / std::mem::size_of::<T>() {
        return Err(invalid(format!("{name} is too long")));
    }
    // SAFETY: The caller promises `length` writable `T` values at `pointer`, with
    // exclusive access for this call.
    Ok(unsafe { std::slice::from_raw_parts_mut(pointer, length) })
}

#[allow(unsafe_code)]
unsafe fn utf8(pointer: *const u8, length: usize, name: &str) -> Result<String, Error> {
    // SAFETY: This function passes through its caller's byte-slice precondition.
    let bytes = unsafe { input_slice(pointer, length, name)? };
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| invalid(format!("{name} is not UTF-8")))
}

#[allow(unsafe_code)]
unsafe fn class_id(pointer: *const u8) -> Result<Option<[u8; 16]>, Error> {
    if pointer.is_null() {
        return Ok(None);
    }
    // SAFETY: A non-null class ID is documented to point to exactly 16 readable bytes.
    let bytes = unsafe { input_slice(pointer, 16, "class_id")? };
    Ok(Some(bytes.try_into().expect("length checked")))
}

fn sample_format_value(format: SampleFormat) -> i32 {
    match format {
        SampleFormat::F32 => 0,
        SampleFormat::F64 => 1,
    }
}

fn bool_byte(value: bool) -> u8 {
    u8::from(value)
}

fn duration(milliseconds: u32) -> Duration {
    Duration::from_millis(u64::from(milliseconds))
}

/// # Safety
/// `buffer` must point to at least `capacity` writable bytes when `capacity` is
/// nonzero.
#[allow(unsafe_code)]
unsafe fn last_error_copy(buffer: *mut u8, capacity: usize, kind: bool) -> usize {
    let bytes = LAST_ERROR.with(|last| {
        let last = last.borrow();
        if kind {
            last.kind.as_bytes().to_vec()
        } else {
            last.message.as_bytes().to_vec()
        }
    });
    let length = bytes.len();
    if capacity == 0 {
        return length;
    }
    if buffer.is_null() {
        set_last_error("buffer is null with nonzero capacity", "invalid_argument");
        return 0;
    }
    let count = capacity.min(length);
    // SAFETY: The caller promises `capacity` writable bytes and `count <= capacity`.
    #[allow(unsafe_code)]
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, count);
    }
    length
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn vsta_abi_version() -> u32 {
    match catch_unwind(|| 1) {
        Ok(version) => version,
        Err(payload) => report_panic(payload) as u32,
    }
}

/// # Safety
/// `buf` must be null when `cap` is zero, or point to at least `cap` writable bytes.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_last_error(buf: *mut u8, cap: usize) -> usize {
    // SAFETY: This function's contract is `last_error_copy`'s.
    match catch_unwind(AssertUnwindSafe(|| unsafe {
        last_error_copy(buf, cap, false)
    })) {
        Ok(length) => length,
        Err(payload) => report_panic(payload) as usize,
    }
}

/// # Safety
/// `buf` must be null when `cap` is zero, or point to at least `cap` writable bytes.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_last_error_kind(buf: *mut u8, cap: usize) -> usize {
    // SAFETY: This function's contract is `last_error_copy`'s.
    match catch_unwind(AssertUnwindSafe(|| unsafe {
        last_error_copy(buf, cap, true)
    })) {
        Ok(length) => length,
        Err(payload) => report_panic(payload) as usize,
    }
}

/// # Safety
/// `worker` must point to `worker_len` readable bytes. `out` must be a valid,
/// exclusively writable pointer. The returned handle must be freed once.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_host_new(
    worker: *const u8,
    worker_len: usize,
    out: *mut *mut VstaHost,
) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers both pointers.
        let worker = unsafe { utf8(worker, worker_len, "worker")? };
        // SAFETY: The function's safety contract grants exclusive output access.
        let out = unsafe { exclusive(out, "out")? };
        *out = Box::into_raw(Box::new(VstaHost(HostHandle::new(PathBuf::from(worker)))));
        Ok(())
    })
}

/// # Safety
/// `host` must be null or a handle returned by `vsta_host_new` that has not been
/// freed. A non-null handle must not be used again after this call.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_host_free(host: *mut VstaHost) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if !host.is_null() {
            // SAFETY: The function contract gives this call sole ownership of the box.
            unsafe { drop(Box::from_raw(host)) };
        }
    }));
    if let Err(payload) = result {
        report_panic(payload);
    }
}

/// # Safety
/// `host` must be a live handle, `path` must point to `path_len` readable bytes,
/// and `out` must be exclusively writable. The host must not be freed concurrently.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_host_scan(
    host: *const VstaHost,
    path: *const u8,
    path_len: usize,
    format: i32,
    sandboxed: u8,
    out: *mut *mut VstaClassList,
) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers these pointers.
        let host = unsafe { shared(host, "host")? };
        // SAFETY: The function's safety contract covers the path bytes.
        let path = unsafe { utf8(path, path_len, "path")? };
        // SAFETY: The function's safety contract grants exclusive output access.
        let out = unsafe { exclusive(out, "out")? };
        let classes = host.0.scan(PathBuf::from(path), format, sandboxed != 0)?;
        *out = Box::into_raw(Box::new(VstaClassList(classes)));
        Ok(())
    })
}

/// # Safety
/// `list` must be null or a live class-list handle and must not be freed concurrently.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_class_list_len(list: *const VstaClassList) -> usize {
    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: The function's safety contract covers `list`.
        unsafe { shared(list, "list") }.map(|list| list.0.len())
    })) {
        Ok(Ok(length)) => length,
        Ok(Err(error)) => {
            report_error(error);
            0
        }
        Err(payload) => report_panic(payload) as usize,
    }
}

/// # Safety
/// `list` must be live, `out` must be exclusively writable, and the list must
/// remain live while any returned string pointer is used.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_class_list_get(
    list: *const VstaClassList,
    index: usize,
    out: *mut VstaClassInfo,
) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers both pointers.
        let list = unsafe { shared(list, "list")? };
        // SAFETY: The function's safety contract grants exclusive output access.
        let out = unsafe { exclusive(out, "out")? };
        let class = list
            .0
            .get(index)
            .ok_or_else(|| invalid("class index is out of range"))?;
        *out = VstaClassInfo {
            id: class.id,
            name: class.name.as_ptr(),
            name_len: class.name.len(),
            vendor: class.vendor.as_ptr(),
            vendor_len: class.vendor.len(),
            category: class.category.as_ptr(),
            category_len: class.category.len(),
        };
        Ok(())
    })
}

/// # Safety
/// `list` must be null or a live class-list handle. A non-null handle must not be
/// used again after this call.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_class_list_free(list: *mut VstaClassList) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if !list.is_null() {
            // SAFETY: The function contract gives this call sole ownership of the box.
            unsafe { drop(Box::from_raw(list)) };
        }
    }));
    if let Err(payload) = result {
        report_panic(payload);
    }
}

/// # Safety
/// `host` must be live; `path` must point to `path_len` readable bytes;
/// `class_id` must be null or point to 16 readable bytes; `out` must be
/// exclusively writable. Handles and pointed-to storage must not be freed during
/// the call. The returned plugin handle must be freed once.
#[allow(clippy::too_many_arguments, unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_host_load(
    host: *const VstaHost,
    path: *const u8,
    path_len: usize,
    format: i32,
    class_id_pointer: *const u8,
    sample_rate: u32,
    block_size: u32,
    sample_format: i32,
    timeout_ms: u32,
    out: *mut *mut VstaPlugin,
) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers these pointers.
        let host = unsafe { shared(host, "host")? };
        // SAFETY: The function's safety contract covers the path bytes.
        let path = unsafe { utf8(path, path_len, "path")? };
        // SAFETY: The function contract requires 16 bytes for a non-null ID.
        let class_id = unsafe { class_id(class_id_pointer)? };
        // SAFETY: The function's safety contract grants exclusive output access.
        let out = unsafe { exclusive(out, "out")? };
        let plugin = host.0.load(
            PathBuf::from(path),
            format,
            class_id,
            sample_rate,
            block_size,
            sample_format,
            duration(timeout_ms),
        )?;
        *out = Box::into_raw(Box::new(VstaPlugin(plugin)));
        Ok(())
    })
}

/// # Safety
/// `plugin` must be null or a live plugin handle. A non-null handle must not be
/// used again after this call. No other call may use it concurrently.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_plugin_free(plugin: *mut VstaPlugin) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if !plugin.is_null() {
            // SAFETY: The function contract gives this call sole ownership of the box.
            unsafe { drop(Box::from_raw(plugin)) };
        }
    }));
    if let Err(payload) = result {
        report_panic(payload);
    }
}

/// # Safety
/// `plugin` must be live and `out` must be exclusively writable. The plugin must
/// not be freed or mutably accessed concurrently.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_plugin_info(
    plugin: *const VstaPlugin,
    out: *mut VstaPluginInfo,
) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers both pointers.
        let plugin = unsafe { shared(plugin, "plugin")? };
        // SAFETY: The function's safety contract grants exclusive output access.
        let out = unsafe { exclusive(out, "out")? };
        let info = plugin.0.info();
        *out = VstaPluginInfo {
            input_channels: info.input_channels,
            output_channels: info.output_channels,
            sample_rate: info.sample_rate,
            block_size: info.block_size,
            sample_format: sample_format_value(info.sample_format),
            latency: info.latency,
        };
        Ok(())
    })
}

/// # Safety
/// `plugin` must be live and exclusively accessible. `input` and `output` must
/// point to their declared numbers of readable and writable samples, respectively,
/// and must not overlap storage that Rust may access through another reference.
#[allow(clippy::too_many_arguments, unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_plugin_process_f32(
    plugin: *mut VstaPlugin,
    input: *const f32,
    input_len: usize,
    frames: usize,
    tail: usize,
    output: *mut f32,
    output_len: usize,
    timeout_ms: u32,
) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers all three pointers.
        let plugin = unsafe { exclusive(plugin, "plugin")? };
        // SAFETY: The function contract provides `input_len` readable samples.
        let input = unsafe { input_slice(input, input_len, "input")? };
        // SAFETY: The function contract provides `output_len` writable samples.
        let output = unsafe { output_slice(output, output_len, "output")? };
        plugin
            .0
            .process_f32(input, frames, tail, output, duration(timeout_ms))
    })
}

/// # Safety
/// `plugin` must be live and exclusively accessible. `input` and `output` must
/// point to their declared numbers of readable and writable samples, respectively,
/// and must not overlap storage that Rust may access through another reference.
#[allow(clippy::too_many_arguments, unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_plugin_process_f64(
    plugin: *mut VstaPlugin,
    input: *const f64,
    input_len: usize,
    frames: usize,
    tail: usize,
    output: *mut f64,
    output_len: usize,
    timeout_ms: u32,
) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers all three pointers.
        let plugin = unsafe { exclusive(plugin, "plugin")? };
        // SAFETY: The function contract provides `input_len` readable samples.
        let input = unsafe { input_slice(input, input_len, "input")? };
        // SAFETY: The function contract provides `output_len` writable samples.
        let output = unsafe { output_slice(output, output_len, "output")? };
        plugin
            .0
            .process_f64(input, frames, tail, output, duration(timeout_ms))
    })
}

/// # Safety
/// `plugin` must be live and exclusively accessible; `out` must be exclusively
/// writable. The returned list must be freed once.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_plugin_parameters(
    plugin: *mut VstaPlugin,
    timeout_ms: u32,
    out: *mut *mut VstaParameterList,
) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers both pointers.
        let plugin = unsafe { exclusive(plugin, "plugin")? };
        // SAFETY: The function's safety contract grants exclusive output access.
        let out = unsafe { exclusive(out, "out")? };
        let parameters = plugin.0.parameters(duration(timeout_ms))?;
        *out = Box::into_raw(Box::new(VstaParameterList(parameters)));
        Ok(())
    })
}

/// # Safety
/// `list` must be null or a live parameter-list handle and must not be freed
/// concurrently.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_parameter_list_len(list: *const VstaParameterList) -> usize {
    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: The function's safety contract covers `list`.
        unsafe { shared(list, "list") }.map(|list| list.0.len())
    })) {
        Ok(Ok(length)) => length,
        Ok(Err(error)) => {
            report_error(error);
            0
        }
        Err(payload) => report_panic(payload) as usize,
    }
}

/// # Safety
/// `list` must be live, `out` must be exclusively writable, and the list must
/// remain live while any returned string pointer is used.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_parameter_list_get(
    list: *const VstaParameterList,
    index: usize,
    out: *mut VstaParameter,
) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers both pointers.
        let list = unsafe { shared(list, "list")? };
        // SAFETY: The function's safety contract grants exclusive output access.
        let out = unsafe { exclusive(out, "out")? };
        let parameter = list
            .0
            .get(index)
            .ok_or_else(|| invalid("parameter index is out of range"))?;
        let (default_value, has_default) = parameter.default.map_or((0.0, 0), |value| (value, 1));
        let (steps, has_steps) = parameter.steps.map_or((0, 0), |value| (value, 1));
        *out = VstaParameter {
            id: parameter.id,
            name: parameter.name.as_ptr(),
            name_len: parameter.name.len(),
            unit: parameter.unit.as_ptr(),
            unit_len: parameter.unit.len(),
            value: parameter.value,
            default_value,
            has_default,
            steps,
            has_steps,
            automatable: bool_byte(parameter.automatable),
            read_only: bool_byte(parameter.read_only),
            hidden: bool_byte(parameter.hidden),
        };
        Ok(())
    })
}

/// # Safety
/// `list` must be null or a live parameter-list handle. A non-null handle must
/// not be used again after this call.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_parameter_list_free(list: *mut VstaParameterList) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if !list.is_null() {
            // SAFETY: The function contract gives this call sole ownership of the box.
            unsafe { drop(Box::from_raw(list)) };
        }
    }));
    if let Err(payload) = result {
        report_panic(payload);
    }
}

/// # Safety
/// `plugin` must be live and exclusively accessible for this call.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_plugin_set_parameter(
    plugin: *mut VstaPlugin,
    id: u32,
    value: f64,
    timeout_ms: u32,
) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers `plugin`.
        let plugin = unsafe { exclusive(plugin, "plugin")? };
        plugin.0.set_parameter(id, value, duration(timeout_ms))
    })
}

/// # Safety
/// `plugin` must be live and exclusively accessible; `out` must be exclusively
/// writable. The returned bytes handle must be freed once.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_plugin_save_state(
    plugin: *mut VstaPlugin,
    timeout_ms: u32,
    out: *mut *mut VstaBytes,
) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers both pointers.
        let plugin = unsafe { exclusive(plugin, "plugin")? };
        // SAFETY: The function's safety contract grants exclusive output access.
        let out = unsafe { exclusive(out, "out")? };
        let bytes = plugin.0.save_state(duration(timeout_ms))?;
        *out = Box::into_raw(Box::new(VstaBytes(bytes)));
        Ok(())
    })
}

/// # Safety
/// `bytes` must be a live bytes handle and remain live while the returned pointer
/// is used. It must not be freed concurrently.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_bytes_data(bytes: *const VstaBytes) -> *const u8 {
    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: The function's safety contract covers `bytes`.
        unsafe { shared(bytes, "bytes") }.map(|bytes| bytes.0.as_ptr())
    })) {
        Ok(Ok(data)) => data,
        Ok(Err(error)) => {
            report_error(error);
            ptr::null()
        }
        Err(payload) => {
            report_panic(payload);
            ptr::null()
        }
    }
}

/// # Safety
/// `bytes` must be a live bytes handle and must not be freed concurrently.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_bytes_len(bytes: *const VstaBytes) -> usize {
    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: The function's safety contract covers `bytes`.
        unsafe { shared(bytes, "bytes") }.map(|bytes| bytes.0.len())
    })) {
        Ok(Ok(length)) => length,
        Ok(Err(error)) => {
            report_error(error);
            0
        }
        Err(payload) => report_panic(payload) as usize,
    }
}

/// # Safety
/// `bytes` must be null or a live bytes handle. A non-null handle must not be
/// used again after this call.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_bytes_free(bytes: *mut VstaBytes) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if !bytes.is_null() {
            // SAFETY: The function contract gives this call sole ownership of the box.
            unsafe { drop(Box::from_raw(bytes)) };
        }
    }));
    if let Err(payload) = result {
        report_panic(payload);
    }
}

/// # Safety
/// `plugin` must be live and exclusively accessible. `data` must point to `len`
/// readable bytes for this call.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_plugin_restore_state(
    plugin: *mut VstaPlugin,
    data: *const u8,
    len: usize,
    timeout_ms: u32,
) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers both pointers.
        let plugin = unsafe { exclusive(plugin, "plugin")? };
        // SAFETY: The function contract provides `len` readable bytes.
        let data = unsafe { input_slice(data, len, "data")? };
        plugin.0.restore_state(data, duration(timeout_ms))
    })
}

/// # Safety
/// `plugin` must be live and exclusively accessible for this call.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_plugin_open_editor(plugin: *mut VstaPlugin, timeout_ms: u32) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers `plugin`.
        let plugin = unsafe { exclusive(plugin, "plugin")? };
        plugin.0.open_editor(duration(timeout_ms))
    })
}

/// # Safety
/// `plugin` must be live and exclusively accessible for this call.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vsta_plugin_close_editor(plugin: *mut VstaPlugin, timeout_ms: u32) -> i32 {
    status_call(|| {
        // SAFETY: The function's safety contract covers `plugin`.
        let plugin = unsafe { exclusive(plugin, "plugin")? };
        plugin.0.close_editor(duration(timeout_ms))
    })
}
