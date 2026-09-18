// chrono stub - minimal implementations for std::chrono support
// This provides stub symbols that chrono headers expect but don't need at runtime
// 
// These are linked as external symbols to satisfy the linker when chrono headers
// are included in user code.

#![allow(dead_code)]

#[no_mangle]
pub extern "C" fn dccpp_dur_free(_dur: *mut libc::c_void) {
    // Stub: duration destructor is a no-op for POD types
}

#[no_mangle]
pub extern "C" fn dccpp_tp_free(_tp: *mut libc::c_void) {
    // Stub: time_point destructor is a no-op for POD types
}

#[no_mangle]
pub extern "C" fn dccpp_sysclock_now() -> *mut libc::c_void {
    // Stub: return dummy pointer for system_clock::now() calls
    std::ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn dccpp_steadyclock_now() -> *mut libc::c_void {
    // Stub: return dummy pointer for steady_clock::now() calls
    std::ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn dccpp_hrclock_now() -> *mut libc::c_void {
    // Stub: return dummy pointer for high_resolution_clock::now() calls
    std::ptr::null_mut()
}
