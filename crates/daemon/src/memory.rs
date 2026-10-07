//! Handing freed memory back to the system.

/// Asks the allocator to give memory freed in the middle of its heap back
/// to the system.
///
/// On its own, glibc's malloc only returns memory from the top of its heap.
/// Compacting scrollback frees thousands of rows from all over it, so
/// without this the daemon's resident memory would stay at its peak.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) fn release_free_memory() {
    // SAFETY: `malloc_trim` has no preconditions. It takes the allocator's
    // own lock, so it's safe to call at any time from any thread.
    #[allow(unsafe_code)]
    unsafe extern "C" {
        safe fn malloc_trim(pad: usize) -> std::ffi::c_int;
    }
    malloc_trim(0);
}

/// Other allocators return memory as they see fit.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(crate) fn release_free_memory() {}
