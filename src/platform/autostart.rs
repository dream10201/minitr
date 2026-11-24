//! Start at logon through a Task Scheduler logon task. The installer registers the
//! same task, so both toggle one entry.

use anyhow::Result;
use windows::Win32::Foundation::VARIANT_FALSE;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::TaskScheduler::{
    IExecAction, ILogonTrigger, ITaskFolder, ITaskService, TASK_ACTION_EXEC, TASK_CREATE_OR_UPDATE,
    TASK_LOGON_INTERACTIVE_TOKEN, TASK_TRIGGER_LOGON, TaskScheduler,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{BSTR, Interface as _};

const TASK_NAME: &str = "MiniTranslation";

fn root_folder() -> windows::core::Result<(ITaskService, ITaskFolder)> {
    unsafe {
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        let none = VARIANT::default();
        service.Connect(&none, &none, &none, &none)?;
        let folder = service.GetFolder(&BSTR::from("\\"))?;
        Ok((service, folder))
    }
}

pub fn is_enabled() -> bool {
    root_folder().is_ok_and(|(_, folder)| unsafe { folder.GetTask(&BSTR::from(TASK_NAME)) }.is_ok())
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    let (service, folder) = root_folder()?;
    let name = BSTR::from(TASK_NAME);
    unsafe {
        if !enabled {
            // Deleting a task that does not exist is not an error worth reporting.
            let _ = folder.DeleteTask(&name, 0);
            return Ok(());
        }
        let task = service.NewTask(0)?;
        let settings = task.Settings()?;
        // Unlimited run time, and laptops on battery still start it.
        settings.SetExecutionTimeLimit(&BSTR::from("PT0S"))?;
        settings.SetDisallowStartIfOnBatteries(VARIANT_FALSE)?;
        settings.SetStopIfGoingOnBatteries(VARIANT_FALSE)?;

        let trigger: ILogonTrigger = task.Triggers()?.Create(TASK_TRIGGER_LOGON)?.cast()?;
        trigger.SetUserId(&BSTR::from(current_user()))?;
        let action: IExecAction = task.Actions()?.Create(TASK_ACTION_EXEC)?.cast()?;
        action.SetPath(&BSTR::from(
            std::env::current_exe()?
                .as_os_str()
                .to_string_lossy()
                .as_ref(),
        ))?;

        let none = VARIANT::default();
        folder.RegisterTaskDefinition(
            &name,
            &task,
            TASK_CREATE_OR_UPDATE.0,
            &none,
            &none,
            TASK_LOGON_INTERACTIVE_TOKEN,
            &none,
        )?;
    }
    Ok(())
}

fn current_user() -> String {
    let var = |name| std::env::var(name).unwrap_or_default();
    format!("{}\\{}", var("USERDOMAIN"), var("USERNAME"))
}
