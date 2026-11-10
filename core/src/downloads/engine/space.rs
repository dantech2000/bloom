// SPDX-License-Identifier: AGPL-3.0-or-later
//! Room for a download: the reserve of free space, the storage limit,
//! and the free space of the disk.

use super::*;

/// Refuses a download that would leave the disk under its reserve, or that
/// would go over the storage limit. The check and the admission are one
/// step under the lock: two downloads at once see each other's size. A
/// download whose size the server did not say is not admitted here; its
/// bytes are checked as they come (`check_budget`).
pub(super) fn check_space(inner: &Arc<Inner>, job: &Job, total: Option<u64>, part_len: u64) -> Result<(), Stop> {
    let Some(total) = total else { return Ok(()) };
    let options = &job.options;
    let need = total.saturating_sub(part_len);
    // The disk is asked before the lock is taken.
    let free = options.free_override.or_else(|| free_space(&job.folder));
    let mut s = inner.lock();
    let used = s.used_except(&job.item_id) + part_len;
    let promised = s.promised_except(&job.item_id);
    if let Some(free) = free
        && free < need.saturating_add(promised).saturating_add(MIN_FREE_BYTES)
    {
        return Err(Stop::Refused(format!(
            "Not enough free space: {} needed, {} free, and {} must stay free",
            format_bytes(need.saturating_add(promised)),
            format_bytes(free),
            format_bytes(MIN_FREE_BYTES)
        )));
    }
    if let Some(limit) = options.limit_bytes
        && used.saturating_add(need) > limit
    {
        return Err(Stop::Refused(format!(
            "Over the storage limit of {} ({} in use)",
            format_bytes(limit),
            format_bytes(used)
        )));
    }
    if let Some(entry) = s.owned_mut(job) {
        entry.admitted = true;
    }
    Ok(())
}

/// The limit and the reserve for a download of unknown size, checked as
/// its bytes come, before `more` bytes go to the part file of `done`
/// bytes: it stops with a clear word when they would cross one of them.
pub(super) fn check_budget(inner: &Arc<Inner>, job: &Job, done: u64, more: u64) -> Result<(), Stop> {
    let options = &job.options;
    let free = options.free_override.or_else(|| free_space(&job.folder));
    let s = inner.lock();
    if let Some(limit) = options.limit_bytes {
        let used = s.used_except(&job.item_id);
        if used.saturating_add(done).saturating_add(more) > limit {
            return Err(Stop::Refused(format!(
                "Over the storage limit of {} ({} in use)",
                format_bytes(limit),
                format_bytes(used)
            )));
        }
    }
    let promised = s.promised_except(&job.item_id);
    if let Some(free) = free
        && free < more.saturating_add(promised).saturating_add(MIN_FREE_BYTES)
    {
        return Err(Stop::Refused(format!(
            "Not enough free space: {} free, and {} must stay free",
            format_bytes(free),
            format_bytes(promised + MIN_FREE_BYTES)
        )));
    }
    Ok(())
}

/// Bytes a user may still write on the disk that holds `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        // The layout of `struct statvfs` on Darwin (macOS and iOS): the block
        // counts are 32 bits there (`__darwin_fsblkcnt_t` is an unsigned int).
        // With the other layout the fields read garbage, and the multiply
        // below overflowed on 2026-10-21 (the iOS build checks overflows).
        #[cfg(target_vendor = "apple")]
        #[repr(C)]
        struct Statvfs {
            f_bsize: std::ffi::c_ulong,
            f_frsize: std::ffi::c_ulong,
            f_blocks: u32,
            f_bfree: u32,
            f_bavail: u32,
            f_files: u32,
            f_ffree: u32,
            f_favail: u32,
            f_fsid: std::ffi::c_ulong,
            f_flag: std::ffi::c_ulong,
            f_namemax: std::ffi::c_ulong,
        }
        #[cfg(not(target_vendor = "apple"))]
        #[repr(C)]
        struct Statvfs {
            f_bsize: std::ffi::c_ulong,
            f_frsize: std::ffi::c_ulong,
            f_blocks: u64,
            f_bfree: u64,
            f_bavail: u64,
            f_files: u64,
            f_ffree: u64,
            f_favail: u64,
            f_fsid: std::ffi::c_ulong,
            f_flag: std::ffi::c_ulong,
            f_namemax: std::ffi::c_ulong,
            f_spare: [std::ffi::c_int; 6],
        }
        unsafe extern "C" {
            fn statvfs(path: *const std::ffi::c_char, buf: *mut Statvfs) -> std::ffi::c_int;
        }
        // The folder may not exist yet; its nearest parent is on the same disk.
        let mut probe = path.to_path_buf();
        while !probe.exists() {
            probe = probe.parent()?.to_path_buf();
        }
        let c_path = std::ffi::CString::new(probe.as_os_str().as_bytes()).ok()?;
        let mut buf = std::mem::MaybeUninit::<Statvfs>::uninit();
        let code = unsafe { statvfs(c_path.as_ptr(), buf.as_mut_ptr()) };
        if code != 0 {
            return None;
        }
        let buf = unsafe { buf.assume_init() };
        (buf.f_bavail as u64).checked_mul(buf.f_frsize as u64)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}
