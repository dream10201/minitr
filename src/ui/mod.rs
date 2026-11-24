mod popup;
mod preferences;
mod tray;

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui_kit::component::Theme;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, AssetSource, Bounds, Entity, Global, QuitMode,
    SharedString, Task, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, px, size,
};

use crate::platform::trigger;
use crate::router::Router;
use crate::settings::Settings;
use crate::update::{self, Pending};
use crate::{runtime, translate};

use popup::Popup;
use preferences::Preferences;

/// Everything the tray, the global trigger and the windows ask the application to do.
#[derive(Debug)]
pub enum Command {
    Summon,
    Show,
    OpenSettings,
    SettingsClosed,
    Quit,
}

/// Saved settings, shared read-only by the windows.
struct Config(Settings);
impl Global for Config {}

struct Services {
    router: Arc<Router>,
    commands: UnboundedSender<Command>,
}
impl Global for Services {}

/// A downloaded release waiting for a restart.
#[derive(Default)]
struct UpdateState(Option<Pending>);
impl Global for UpdateState {}

fn config(cx: &App) -> &Settings {
    &cx.global::<Config>().0
}

fn send(cx: &App, command: Command) {
    let _ = cx.global::<Services>().commands.unbounded_send(command);
}

fn router(cx: &App) -> Arc<Router> {
    cx.global::<Services>().router.clone()
}

/// Installs the downloaded release, after a UAC prompt when the install directory
/// needs it, then hands over to the new executable.
fn install_update(pending: Pending, cx: &mut App) -> Task<anyhow::Result<()>> {
    cx.spawn(async move |cx| {
        let installed = cx.background_spawn(async move { update::install(&pending) });
        let exe = installed
            .await
            .inspect_err(|error| log::error!("{error:#}"))?;
        std::process::Command::new(&exe)
            .spawn()
            .context("无法启动新版本")?;
        cx.update(|cx| send(cx, Command::Quit));
        Ok(())
    })
}

gpui_kit::assets::icon_assets!(ExtraIcons, [Volume2, Pin, PinOff, Keyboard, Mouse]);

struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

pub fn run() {
    gpui_kit::application()
        .with_assets(Assets)
        .with_quit_mode(QuitMode::Explicit)
        .run(|cx| {
            gpui_kit::init(cx);
            gpui_kit::component::set_locale("zh-CN");
            popup::bind_keys(cx);
            Theme::sync_system_appearance(None, cx);

            let settings = Settings::load();
            let (commands, mut inbox) = unbounded();
            cx.set_global(Services {
                router: Router::new(translate::http_client(), &settings.profiles),
                commands: commands.clone(),
            });
            cx.set_global(Config(settings));
            cx.set_global(UpdateState(
                update::enabled().then(update::pending).flatten(),
            ));

            let tray = match tray::install(commands) {
                Ok(tray) => Some(tray),
                Err(error) => {
                    log::error!("tray icon unavailable: {error:#}");
                    None
                }
            };
            let mut controller = Controller {
                popup: None,
                preferences: None,
                listener: None,
                tray,
            };
            if let Err(error) = controller.arm(cx) {
                controller.open_preferences(Some(format!("{error:#}")), cx);
            }
            cx.spawn(async move |cx| {
                while let Some(command) = inbox.next().await {
                    cx.update(|cx| controller.handle(command, cx));
                }
            })
            .detach();
            cx.spawn(async move |cx| update_loop(cx).await).detach();
        });
}

struct Controller {
    popup: Option<(AnyWindowHandle, Entity<Popup>)>,
    preferences: Option<AnyWindowHandle>,
    listener: Option<trigger::Listener>,
    tray: Option<tray_icon::TrayIcon>,
}

impl Controller {
    fn handle(&mut self, command: Command, cx: &mut App) {
        match command {
            Command::Summon => self.with_popup(cx, Popup::summon),
            Command::Show => self.with_popup(cx, |popup, window, cx| popup.show(true, window, cx)),
            Command::OpenSettings => self.open_preferences(None, cx),
            Command::SettingsClosed => {
                self.preferences = None;
                // A failure is already reported through the tray tooltip.
                let _ = self.arm(cx);
            }
            Command::Quit => {
                self.listener = None;
                self.tray = None;
                // The framework's quit does not always end the run loop, and an
                // updated instance waits for this process to exit.
                std::process::exit(0);
            }
        }
    }

    /// (Re)installs the global trigger from the saved settings.
    fn arm(&mut self, cx: &mut App) -> anyhow::Result<()> {
        self.listener = None;
        let settings = config(cx);
        let interval = settings.double_press_interval().as_millis() as u32;
        let commands = cx.global::<Services>().commands.clone();
        let listener = trigger::listen(settings.hot_key, interval, move || {
            let _ = commands.unbounded_send(Command::Summon);
        });
        if let Some(tray) = &self.tray {
            let tooltip = match &listener {
                Ok(_) => format!("MiniTranslation（{}）", settings.hot_key.label()),
                Err(error) => format!("MiniTranslation\n{error:#}"),
            };
            let _ = tray.set_tooltip(Some(tooltip));
        }
        self.listener = Some(listener.inspect_err(|error| log::error!("{error:#}"))?);
        Ok(())
    }

    fn with_popup(
        &mut self,
        cx: &mut App,
        action: impl FnOnce(&mut Popup, &mut gpui_kit::Window, &mut gpui_kit::Context<Popup>),
    ) {
        if self.preferences.is_some() {
            self.focus_preferences(cx);
            return;
        }
        let (window, popup) = match &self.popup {
            Some(existing) => existing.clone(),
            None => match open_popup(cx) {
                Ok(created) => self.popup.insert(created).clone(),
                Err(error) => {
                    log::error!("cannot open the translator window: {error:#}");
                    return;
                }
            },
        };
        let _ = window.update(cx, |_, window, cx| {
            popup.update(cx, |popup, cx| action(popup, window, cx))
        });
    }

    fn open_preferences(&mut self, notice: Option<String>, cx: &mut App) {
        if self.preferences.is_some() {
            self.focus_preferences(cx);
            return;
        }
        // Recording a new trigger must not fire the current one; the popup would
        // also cover the dialog, being topmost.
        self.listener = None;
        if let Some((window, popup)) = &self.popup {
            let _ = window.update(cx, |_, window, cx| {
                popup.update(cx, |popup, cx| popup.hide(window, cx))
            });
        }
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("MiniTranslation 设置".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(560.), px(680.)),
                cx,
            ))),
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        };
        match gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Preferences::new(notice, window, cx))
        }) {
            Ok((window, _)) => self.preferences = Some(window),
            Err(error) => {
                log::error!("cannot open settings: {error:#}");
                let _ = self.arm(cx);
            }
        }
    }

    fn focus_preferences(&self, cx: &mut App) {
        if let Some(window) = self.preferences {
            let _ = window.update(cx, |_, window, _| window.activate_window());
        }
    }
}

fn open_popup(cx: &mut App) -> anyhow::Result<(AnyWindowHandle, Entity<Popup>)> {
    let options = WindowOptions {
        titlebar: None,
        kind: WindowKind::PopUp,
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(popup::WIDTH), px(popup::MIN_HEIGHT)),
            cx,
        ))),
        show: false,
        focus: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Opaque,
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Popup::new(window, cx))
    })
}

/// Checks shortly after start, then hourly; a found release is downloaded silently
/// and offered for restart.
async fn update_loop(cx: &mut gpui_kit::AsyncApp) {
    if !update::enabled() {
        return;
    }
    let timer = |duration| cx.background_executor().timer(duration);
    timer(Duration::from_secs(10)).await;
    loop {
        let (enabled, client) =
            cx.update(|cx| (config(cx).auto_check_update, router(cx).client().clone()));
        if enabled {
            match runtime::spawn(async move { update::check_and_download(&client, |_| {}).await })
                .await
            {
                Ok(Some(pending)) => cx.update(|cx| {
                    cx.set_global(UpdateState(Some(pending)));
                    cx.refresh_windows();
                }),
                Ok(None) => {}
                Err(error) => log::warn!("update check failed: {error:#}"),
            }
        }
        timer(Duration::from_secs(60 * 60)).await;
    }
}
