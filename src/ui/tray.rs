use anyhow::Result;
use futures::channel::mpsc::UnboundedSender;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};

use super::Command;

/// The icon lives as long as the returned handle.
pub fn install(commands: UnboundedSender<Command>) -> Result<TrayIcon> {
    let show = MenuItem::new("显示", true, None);
    let settings = MenuItem::new("设置", true, None);
    let quit = MenuItem::new("退出", true, None);
    let menu = Menu::with_items(&[&show, &settings, &PredefinedMenuItem::separator(), &quit])?;

    let ids = [show.id().clone(), settings.id().clone(), quit.id().clone()];
    let sender = commands.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let command = match ids.iter().position(|id| *id == event.id) {
            Some(0) => Command::Show,
            Some(1) => Command::OpenSettings,
            Some(2) => Command::Quit,
            _ => return,
        };
        let _ = sender.unbounded_send(command);
    }));
    TrayIconEvent::set_event_handler(Some(move |event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = commands.unbounded_send(Command::Show);
        }
    }));

    let side = unsafe { GetSystemMetrics(SM_CXSMICON) } as u32;
    Ok(TrayIconBuilder::new()
        .with_icon(Icon::from_resource(1, Some((side, side)))?)
        .with_tooltip("MiniTranslation")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?)
}
