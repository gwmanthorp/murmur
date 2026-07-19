//! Raw Win32 clipboard: set text, snapshot/restore common formats
//! (unicode text, DIB images, file lists), and sequence-number checks so a
//! restore never clobbers something the user copied in the meantime.

use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};

const CF_UNICODETEXT: u32 = 13;
const CF_DIB: u32 = 8;
const CF_DIBV5: u32 = 17;
const CF_HDROP: u32 = 15;

/// HGLOBAL-backed formats we snapshot and restore.
const SNAPSHOT_FORMATS: [u32; 4] = [CF_UNICODETEXT, CF_DIB, CF_DIBV5, CF_HDROP];

pub struct ClipboardSnapshot {
    formats: Vec<(u32, Vec<u8>)>,
}

struct OpenClipboardGuard;

impl OpenClipboardGuard {
    /// OpenClipboard fails transiently while another app holds it — retry.
    fn acquire() -> Option<Self> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Some(Self);
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        None
    }
}

impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

pub fn sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

unsafe fn read_hglobal(handle: HANDLE) -> Option<Vec<u8>> {
    let hglobal = HGLOBAL(handle.0);
    let size = GlobalSize(hglobal);
    if size == 0 {
        return None;
    }
    let ptr = GlobalLock(hglobal);
    if ptr.is_null() {
        return None;
    }
    let bytes = std::slice::from_raw_parts(ptr as *const u8, size).to_vec();
    let _ = GlobalUnlock(hglobal);
    Some(bytes)
}

unsafe fn alloc_hglobal(bytes: &[u8]) -> Option<HGLOBAL> {
    let hglobal = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).ok()?;
    let ptr = GlobalLock(hglobal);
    if ptr.is_null() {
        return None;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
    let _ = GlobalUnlock(hglobal);
    Some(hglobal)
}

/// Snapshot the formats we know how to restore.
pub fn snapshot() -> Option<ClipboardSnapshot> {
    let _guard = OpenClipboardGuard::acquire()?;
    let mut formats = Vec::new();
    unsafe {
        let mut format = EnumClipboardFormats(0);
        while format != 0 {
            if SNAPSHOT_FORMATS.contains(&format) {
                if let Ok(handle) = GetClipboardData(format) {
                    if let Some(bytes) = read_hglobal(handle) {
                        formats.push((format, bytes));
                    }
                }
            }
            format = EnumClipboardFormats(format);
        }
    }
    Some(ClipboardSnapshot { formats })
}

pub fn set_text(text: &str) -> bool {
    let _guard = match OpenClipboardGuard::acquire() {
        Some(g) => g,
        None => return false,
    };
    unsafe {
        if EmptyClipboard().is_err() {
            return false;
        }
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
        let Some(hglobal) = alloc_hglobal(bytes) else {
            return false;
        };
        // On success the system owns the HGLOBAL.
        SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hglobal.0))).is_ok()
    }
}

/// Restore a snapshot. Returns false if nothing was restored.
pub fn restore(snapshot: &ClipboardSnapshot) -> bool {
    if snapshot.formats.is_empty() {
        // Original clipboard was empty (or unsupported): just clear ours.
        if let Some(_guard) = OpenClipboardGuard::acquire() {
            unsafe {
                let _ = EmptyClipboard();
            }
            return true;
        }
        return false;
    }
    let _guard = match OpenClipboardGuard::acquire() {
        Some(g) => g,
        None => return false,
    };
    unsafe {
        if EmptyClipboard().is_err() {
            return false;
        }
        let mut any = false;
        for (format, bytes) in &snapshot.formats {
            if let Some(hglobal) = alloc_hglobal(bytes) {
                if SetClipboardData(*format, Some(HANDLE(hglobal.0))).is_ok() {
                    any = true;
                }
            }
        }
        any
    }
}
