use std::path::Path;

use anyhow::{Context as _, Result, bail};
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HSTRING, PCWSTR, w};

/// Starts `exe` as administrator, which shows the UAC prompt, and waits for it to
/// exit. Blocks: call it off the UI thread.
pub fn run_as_admin(exe: &Path, parameters: &str) -> Result<u32> {
    let file = HSTRING::from(exe.as_os_str());
    let parameters = HSTRING::from(parameters);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        // Shell execution may load shell extensions, which expect a COM apartment.
        let initialized =
            CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).is_ok();
        let started = ShellExecuteExW(&mut info);
        if initialized {
            CoUninitialize();
        }
        if let Err(error) = started {
            if error.code() == ERROR_CANCELLED.to_hresult() {
                bail!("需要管理员权限才能安装更新");
            }
            return Err(error).context("无法以管理员身份运行");
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 0;
        let exited = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        exited.context("无法读取安装结果")?;
        Ok(code)
    }
}
