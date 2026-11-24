use futures::channel::oneshot;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationTextPattern, UIA_TextPatternId,
};

/// Reads the focused control's selection through UI Automation, without touching the
/// clipboard. The target app can stall the call for seconds, so it runs on its own
/// thread and the caller decides how long to wait.
pub fn selected_text() -> oneshot::Receiver<String> {
    let (sender, receiver) = oneshot::channel();
    std::thread::spawn(move || {
        let text = unsafe {
            let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let text = read().unwrap_or_default();
            if initialized {
                CoUninitialize();
            }
            text
        };
        let _ = sender.send(text);
    });
    receiver
}

unsafe fn read() -> windows::core::Result<String> {
    unsafe {
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
        let pattern: IUIAutomationTextPattern = automation
            .GetFocusedElement()?
            .GetCurrentPatternAs(UIA_TextPatternId)?;
        let ranges = pattern.GetSelection()?;
        let mut text = String::new();
        for index in 0..ranges.Length()? {
            text.push_str(&ranges.GetElement(index)?.GetText(-1)?.to_string());
        }
        Ok(text.trim().to_owned())
    }
}
