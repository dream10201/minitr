use std::thread::sleep;
use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// Open clipboard; another process may hold it briefly, so opening retries.
struct Session;

impl Session {
    fn open() -> Option<Self> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(Some(super::message_window())) }.is_ok() {
                return Some(Self);
            }
            sleep(Duration::from_millis(10));
        }
        None
    }

    fn read(&self, format: u32) -> Option<Vec<u8>> {
        let memory = HGLOBAL(unsafe { GetClipboardData(format) }.ok()?.0);
        unsafe {
            let size = GlobalSize(memory);
            let data = GlobalLock(memory) as *const u8;
            if size == 0 || data.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(data, size).to_vec();
            let _ = GlobalUnlock(memory);
            Some(bytes)
        }
    }

    fn write(&self, format: u32, bytes: &[u8]) -> bool {
        unsafe {
            let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) else {
                return false;
            };
            let data = GlobalLock(memory) as *mut u8;
            if data.is_null() {
                let _ = GlobalFree(Some(memory));
                return false;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            let _ = GlobalUnlock(memory);
            // On success the system owns the memory.
            if SetClipboardData(format, Some(HANDLE(memory.0))).is_err() {
                let _ = GlobalFree(Some(memory));
                return false;
            }
            true
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

/// Changes on every clipboard write, even when the content is identical.
pub fn sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

pub fn text() -> Option<String> {
    let bytes = Session::open()?.read(CF_UNICODETEXT.0.into())?;
    Some(decode_utf16(&bytes))
}

pub fn set_text(text: &str) -> bool {
    let Some(session) = Session::open() else {
        return false;
    };
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    unsafe { EmptyClipboard() }.is_ok() && session.write(CF_UNICODETEXT.0.into(), &bytes)
}

fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// A copy of every memory-backed clipboard format.
pub struct Snapshot(Vec<(u32, Vec<u8>)>);

/// Formats whose handles are GDI objects or owner-drawn, not global memory.
fn is_memory_format(format: u32) -> bool {
    !matches!(
        format,
        2 | 3 | 9 | 14 | 0x80 | 0x82 | 0x83 | 0x8E | 0x200..=0x3FF
    )
}

pub fn snapshot() -> Option<Snapshot> {
    let session = Session::open()?;
    let mut formats = Vec::new();
    let mut format = 0;
    loop {
        format = unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            break;
        }
        if is_memory_format(format)
            && let Some(bytes) = session.read(format)
        {
            formats.push((format, bytes));
        }
    }
    (!formats.is_empty()).then_some(Snapshot(formats))
}

impl Snapshot {
    pub fn restore(&self) -> bool {
        let Some(session) = Session::open() else {
            return false;
        };
        if unsafe { EmptyClipboard() }.is_err() {
            return false;
        }
        self.0.iter().fold(true, |ok, (format, bytes)| {
            session.write(*format, bytes) && ok
        })
    }
}
