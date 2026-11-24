use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::w;

/// Held for the lifetime of the process; shared with earlier releases so two
/// generations never run side by side.
pub struct Instance(HANDLE);

pub fn acquire(timeout: Duration) -> Option<Instance> {
    let handle = unsafe { CreateMutexW(None, false, w!("MiniTranslation_SingleInstance")) }.ok()?;
    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    match unsafe { WaitForSingleObject(handle, millis) } {
        WAIT_OBJECT_0 | WAIT_ABANDONED => Some(Instance(handle)),
        _ => {
            unsafe { CloseHandle(handle).ok() };
            None
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}
