use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use futures::channel::mpsc;
use futures::{FutureExt as _, StreamExt as _};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::SelectableText;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _, Theme, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window,
    WindowControlArea, actions, canvas, div, px,
};

use super::{Command, UpdateState, config, restart_into, send};
use crate::platform::clipboard::{self, Snapshot};
use crate::platform::speech::Speaker;
use crate::platform::window::Handle;
use crate::platform::{input, selection};
use crate::runtime;
use crate::translate::{Translation, is_mainly_chinese, normalize_input};

actions!(popup, [Hide, Speak]);

pub const WIDTH: f32 = 460.;
pub const MIN_HEIGHT: f32 = 96.;
const OUTPUT_MAX_HEIGHT: f32 = 420.;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);
const CONTEXT: &str = "Popup";

pub fn bind_keys(cx: &mut App) {
    // Bound on the nested input context too, so they win over its own Tab/Escape.
    cx.bind_keys([
        KeyBinding::new("escape", Hide, Some(CONTEXT)),
        KeyBinding::new("escape", Hide, Some("Popup > Input")),
        KeyBinding::new("tab", Speak, Some(CONTEXT)),
        KeyBinding::new("tab", Speak, Some("Popup > Input")),
    ]);
}

enum Phase {
    Idle,
    Translating,
    Done,
    Failed(SharedString),
}

/// The translator window: input on top, translation below, actions in the footer.
pub struct Popup {
    handle: Option<Handle>,
    input: Entity<TextareaState>,
    phase: Phase,
    /// Normalized text of the current or last translation.
    source: String,
    output: SharedString,
    speak_text: Option<String>,
    notice: Option<SharedString>,
    pinned: bool,
    visible: bool,
    placed: bool,
    speaker: Speaker,
    /// Clipboard sequence number already consumed, so showing the window only
    /// translates content that is new since.
    clipboard_seen: Rc<Cell<u32>>,
    /// Last client size requested from the platform, in physical pixels.
    fitted: Rc<Cell<(i32, i32)>>,
    translation: Option<Task<()>>,
    capture: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Popup {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder("输入文字，回车翻译")
        });
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter { shift: false, .. } => {
                    this.capture = None;
                    this.translate(window, cx);
                }
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active()
                    && this.visible
                    && !this.pinned
                    && config(cx).hide_on_focus_lost
                {
                    this.hide(window, cx);
                }
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
            }),
        ];

        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = view.update(cx, |this, cx| this.hide(window, cx));
            false
        });
        let handle = Handle::of(window);
        if let Some(handle) = handle {
            handle.round_corners();
        }

        Self {
            handle,
            input,
            phase: Phase::Idle,
            source: String::new(),
            output: SharedString::default(),
            speak_text: None,
            notice: None,
            pinned: false,
            visible: false,
            placed: false,
            speaker: Speaker::new(),
            clipboard_seen: Rc::default(),
            fitted: Rc::default(),
            translation: None,
            capture: None,
            _subscriptions: subscriptions,
        }
    }

    /// The global trigger: toggles the window, grabbing the selection on the way in.
    pub fn summon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = config(cx);
        // Visible but behind another app: re-capture instead of hiding.
        if self.visible && self.handle.is_some_and(Handle::is_foreground) {
            self.hide(window, cx);
        } else if settings.auto_translate_selection && settings.is_configured() {
            self.capture_selection(window, cx);
        } else {
            self.show(true, window, cx);
        }
    }

    pub fn show(&mut self, translate_clipboard: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = self.handle {
            handle.show();
        }
        window.activate_window();
        if let Some(handle) = self.handle
            && !std::mem::replace(&mut self.placed, true)
        {
            // Queued behind the activation, which applies the framework's own
            // initial placement first.
            let (width, measured) = self.fitted.get();
            let width = width.max(physical(WIDTH, window));
            let height = measured.max(physical(MIN_HEIGHT, window));
            cx.spawn(async move |_, _| handle.fit(width, height, true))
                .detach();
        }
        self.visible = true;
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        if translate_clipboard {
            self.translate_clipboard(window, cx);
        }
        cx.notify();
    }

    pub fn hide(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.speaker.stop();
        self.capture = None;
        // A half-streamed translation is useless later; a finished one is kept so
        // the same text shows instantly next time.
        if matches!(self.phase, Phase::Translating) {
            self.translation = None;
            self.phase = Phase::Idle;
            self.output = SharedString::default();
        }
        self.notice = None;
        self.visible = false;
        if let Some(handle) = self.handle {
            handle.hide();
        }
        cx.notify();
    }

    fn translate_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = config(cx);
        if !settings.auto_translate_clipboard || !settings.is_configured() {
            return;
        }
        let sequence = clipboard::sequence_number();
        if self.clipboard_seen.replace(sequence) == sequence {
            return;
        }
        if let Some(text) = clipboard::text().filter(|text| !text.trim().is_empty()) {
            self.auto_translate(text.trim(), window, cx);
        }
    }

    /// Reads the selection of the app in front: UI Automation if it answers quickly,
    /// otherwise a simulated Ctrl+C with the user's clipboard put back afterwards.
    fn capture_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        input::mask_modifier_release();
        let trigger = config(cx).hot_key;
        let seen = self.clipboard_seen.clone();
        let executor = cx.background_executor().clone();
        let sleep = move |ms| executor.timer(Duration::from_millis(ms));

        self.capture = Some(cx.spawn_in(window, async move |this, cx| {
            let show = |cx: &mut gpui_kit::AsyncWindowContext| {
                this.update_in(cx, |this, window, cx| this.show(false, window, cx))
                    .is_ok()
            };
            // Must happen before this window takes the focus away from the target.
            let selected = futures::select! {
                text = selection::selected_text().fuse() => text.unwrap_or_default(),
                _ = sleep(300).fuse() => String::new(),
            };
            if !selected.is_empty() {
                show(cx);
                let _ = this.update_in(cx, |this, window, cx| {
                    this.auto_translate(&selected, window, cx)
                });
                return;
            }

            for _ in 0..10 {
                if !input::trigger_held(trigger) {
                    break;
                }
                sleep(30).await;
            }
            let lease = ClipboardLease::take(seen.clone());
            input::send_copy();

            // Only a clipboard write after our Ctrl+C counts: the old content must not
            // be mistaken for the selection. Slow apps get a second Ctrl+C while they
            // still have focus.
            let mut shown = false;
            for attempt in 0..30 {
                if lease.changed() {
                    shown = shown || show(cx);
                    if let Some(text) = clipboard::text().filter(|text| !text.trim().is_empty()) {
                        seen.set(clipboard::sequence_number());
                        let _ = this.update_in(cx, |this, window, cx| {
                            this.auto_translate(text.trim(), window, cx)
                        });
                        break;
                    }
                } else if attempt == 3 {
                    input::send_copy();
                } else if attempt == 6 {
                    shown = show(cx);
                }
                sleep(50).await;
            }
            if !shown {
                show(cx);
            }
        }));
    }

    fn set_input(&self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(text.to_owned(), window, cx);
            input.set_selected_range(text.len()..text.len(), cx);
        });
    }

    /// Reuses the finished translation when the text has not changed.
    fn auto_translate(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_input(text, window, cx);
        if matches!(self.phase, Phase::Done) && normalize_input(text) == self.source {
            cx.notify();
            return;
        }
        self.translate(window, cx);
    }

    fn translate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.input.read(cx).value();
        let text = normalize_input(&raw);
        if text.is_empty() || matches!(self.phase, Phase::Translating) && text == self.source {
            return;
        }
        if text != raw.as_ref() {
            self.set_input(&text, window, cx);
        }
        if !config(cx).is_configured() {
            self.phase = Phase::Failed("请先配置翻译接口（API 地址 / Key / 模型）。".into());
            send(cx, Command::OpenSettings);
            cx.notify();
            return;
        }

        self.source = text.clone();
        self.phase = Phase::Translating;
        self.output = SharedString::default();
        self.speak_text = None;
        self.notice = None;

        let router = super::router(cx);
        let (progress, mut updates) = mpsc::unbounded::<String>();
        let job = runtime::spawn(async move {
            router
                .translate(&text, move |partial| {
                    let _ = progress.unbounded_send(partial.to_owned());
                })
                .await
        });
        let executor = cx.background_executor().clone();
        self.translation = Some(cx.spawn_in(window, async move |this, cx| {
            // The sender lives inside the job, so the stream ends when the job does.
            while let Some(mut partial) = updates.next().await {
                while let Ok(newer) = updates.try_recv() {
                    partial = newer;
                }
                let applied = this.update(cx, |this, cx| {
                    this.output = partial.into();
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
                executor.timer(PROGRESS_INTERVAL).await;
            }
            let result = job.await;
            let _ = this.update(cx, |this, cx| this.finish(result, cx));
        }));
        cx.notify();
    }

    fn finish(&mut self, result: anyhow::Result<Translation>, cx: &mut Context<Self>) {
        match result {
            Ok(translation) => {
                self.speak_text = Some(match translation.source_is_chinese {
                    true => translation.text.clone(),
                    false => self.source.clone(),
                });
                self.output = translation.text.into();
                self.phase = Phase::Done;
                if config(cx).auto_copy_result {
                    self.copy();
                }
            }
            Err(error) => self.phase = Phase::Failed(format!("翻译失败：{error:#}").into()),
        }
        cx.notify();
    }

    fn copy(&mut self) {
        if matches!(self.phase, Phase::Done) && clipboard::set_text(&self.output) {
            // Our own write must not count as new clipboard content.
            self.clipboard_seen.set(clipboard::sequence_number());
            self.notice = Some("已复制".into());
        }
    }

    fn speak(&self) {
        if let Some(text) = &self.speak_text {
            self.speaker.speak(text);
        }
    }

    /// Sizes the native window to the content. The canvas fills a wrapper without
    /// border or padding, so its height is the content's natural height; the width
    /// is fixed and never derived from the window, which would feed back on itself.
    fn measure(&self, window: &Window) -> impl IntoElement {
        let handle = self.handle;
        let fitted = self.fitted.clone();
        let width = physical(WIDTH, window);
        let scale = window.scale_factor();
        canvas(
            move |bounds, _, cx| {
                let height = (bounds.size.height.as_f32() * scale).round() as i32;
                let size = (width, height);
                if let Some(handle) = handle
                    && fitted.replace(size) != size
                {
                    // Deferred to the event loop: resizing re-enters the framework.
                    cx.spawn(async move |_| handle.fit(size.0, size.1, false))
                        .detach();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }

    fn render_output(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let body = match &self.phase {
            Phase::Failed(message) => div().text_color(theme.danger).child(message.clone()),
            _ => div().child(SelectableText::new("translation", self.output.clone())),
        };
        div().px_2().pb_1().child(
            div()
                .rounded(theme.radius)
                .bg(theme.muted)
                .px_3()
                .py_2()
                .max_h(px(OUTPUT_MAX_HEIGHT))
                .overflow_y_scrollbar()
                .child(body),
        )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let text = self.input.read(cx).value();
        let direction = (!text.trim().is_empty()).then(|| match is_mainly_chinese(&text) {
            true => "中 → 英",
            false => "英 → 中",
        });
        let status: Option<SharedString> = match &self.phase {
            Phase::Translating => Some("翻译中…".into()),
            _ => self.notice.clone(),
        };
        let pending = cx.global::<UpdateState>().0.clone();
        let done = matches!(self.phase, Phase::Done);

        h_flex()
            .h(px(36.))
            .px_2()
            .gap_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .when_some(direction, |row, direction| {
                row.child(div().px_1().child(direction))
            })
            .child(
                Button::new("speak")
                    .ghost()
                    .xsmall()
                    .icon(Lucide::Volume2)
                    .tooltip("朗读英文 (Tab)")
                    .disabled(self.speak_text.is_none())
                    .on_click(cx.listener(|this, _, _, _| this.speak())),
            )
            .child(
                Button::new("copy")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Copy)
                    .tooltip("复制译文")
                    .disabled(!done)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.copy();
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            )
            .when_some(status, |row, status| row.child(status))
            .when_some(pending, |row, pending| {
                row.child(
                    Button::new("restart")
                        .ghost()
                        .xsmall()
                        .label(format!("{} 已就绪，重启更新", pending.version))
                        .on_click(move |_, _, cx| {
                            let _ = restart_into(&pending, cx);
                        }),
                )
            })
            .child(
                Button::new("pin")
                    .ghost()
                    .xsmall()
                    .icon(if self.pinned {
                        Lucide::PinOff
                    } else {
                        Lucide::Pin
                    })
                    .tooltip(if self.pinned {
                        "取消固定"
                    } else {
                        "固定：失去焦点时不隐藏"
                    })
                    .selected(self.pinned)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pinned = !this.pinned;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("settings")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Settings)
                    .tooltip("设置")
                    .on_click(|_, _, cx| send(cx, Command::OpenSettings)),
            )
    }
}

impl Render for Popup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let has_output = !self.output.is_empty() || matches!(self.phase, Phase::Failed(_));
        let content = v_flex()
            .w_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .border_1()
            .border_color(theme.border)
            .child(div().h(px(8.)).window_control_area(WindowControlArea::Drag))
            .child(
                div()
                    .px_2()
                    .child(Textarea::new(&self.input).appearance(false)),
            )
            .when(has_output, |root| root.child(self.render_output(cx)))
            .child(self.render_footer(cx));
        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &Hide, window, cx| this.hide(window, cx)))
            .on_action(cx.listener(|this, _: &Speak, _, _| this.speak()))
            .relative()
            .w(px(WIDTH))
            .child(self.measure(window))
            .child(content)
    }
}

fn physical(logical: f32, window: &Window) -> i32 {
    (logical * window.scale_factor()).round() as i32
}

/// Holds the user's clipboard while a simulated copy borrows it and puts it back
/// when dropped, including when the capture is cancelled midway.
struct ClipboardLease {
    backup: Option<Snapshot>,
    sequence: u32,
    seen: Rc<Cell<u32>>,
}

impl ClipboardLease {
    fn take(seen: Rc<Cell<u32>>) -> Self {
        Self {
            backup: clipboard::snapshot(),
            sequence: clipboard::sequence_number(),
            seen,
        }
    }

    fn changed(&self) -> bool {
        clipboard::sequence_number() != self.sequence
    }
}

impl Drop for ClipboardLease {
    fn drop(&mut self) {
        if self.changed()
            && let Some(backup) = &self.backup
            && backup.restore()
        {
            self.seen.set(clipboard::sequence_number());
        }
    }
}
