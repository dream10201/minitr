use std::time::Instant;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, NumberInput};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, IndexPath, Sizable as _, StyledExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, ModifiersChangedEvent, MouseButton,
    MouseDownEvent, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, px,
};

use super::{Command, Config, UpdateState, config, restart_into, router, send};
use crate::hotkey::{Modifier, Trigger};
use crate::platform::{autostart, input, trigger};
use crate::settings::{INTERVAL_RANGE, Profile, Settings};
use crate::translate::Language;
use crate::{runtime, translate, update};

/// Profiles get an identity of their own so rows stay stable while reordered.
struct Entry {
    id: u64,
    profile: Profile,
}

struct Status {
    text: SharedString,
    error: bool,
}

fn toggles(s: &mut Settings) -> [(&'static str, &'static str, &mut bool); 5] {
    [
        (
            "clipboard",
            "显示窗口时自动翻译剪贴板内容",
            &mut s.auto_translate_clipboard,
        ),
        (
            "selection",
            "唤出时自动翻译选中的文字",
            &mut s.auto_translate_selection,
        ),
        ("focus", "失去焦点时自动隐藏窗口", &mut s.hide_on_focus_lost),
        ("copy", "翻译完成后自动复制译文", &mut s.auto_copy_result),
        ("update", "自动下载更新", &mut s.auto_check_update),
    ]
}

/// Settings window. Edits a draft; nothing applies until saved.
pub struct Preferences {
    draft: Settings,
    entries: Vec<Entry>,
    next_id: u64,
    selected: usize,
    url: Entity<InputState>,
    key: Entity<InputState>,
    model: Entity<InputState>,
    interval: Entity<InputState>,
    language: Entity<SelectState<Vec<&'static str>>>,
    recorder: FocusHandle,
    held_modifier: Option<Modifier>,
    released: Option<(Modifier, Instant)>,
    auto_start: bool,
    auto_start_saved: bool,
    status: Option<Status>,
    testing: Option<Task<()>>,
    updating: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Preferences {
    pub fn new(notice: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let draft = config(cx).clone();
        let mut entries: Vec<Entry> = draft
            .profiles
            .iter()
            .cloned()
            .zip(0..)
            .map(|(profile, id)| Entry { id, profile })
            .collect();
        if entries.is_empty() {
            entries.push(Entry {
                id: 0,
                profile: Profile::default(),
            });
        }

        let url = cx.new(|cx| InputState::new(window, cx).placeholder("https://api.deepseek.com"));
        let key = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("sk-…"));
        let model = cx.new(|cx| InputState::new(window, cx).placeholder("deepseek-chat"));
        let interval = cx.new(|cx| {
            InputState::new(window, cx)
                .step(50.)
                .min(f64::from(*INTERVAL_RANGE.start()))
                .max(f64::from(*INTERVAL_RANGE.end()))
                .default_value(draft.double_press_interval().as_millis().to_string())
        });

        let names = Language::ALL.map(Language::name).to_vec();
        let current = Language::ALL
            .iter()
            .position(|l| *l == draft.foreign_language);
        let language = cx.new(|cx| {
            SelectState::new(
                names,
                current.map(|row| IndexPath::default().row(row)),
                window,
                cx,
            )
        });

        let mut subscriptions: Vec<Subscription> = [&url, &key, &model]
            .into_iter()
            .map(|field| {
                cx.subscribe(field, |this, _, event, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.store_fields(cx);
                    }
                })
            })
            .collect();
        subscriptions.push(cx.on_release(|_, cx| send(cx, Command::SettingsClosed)));

        let auto_start = autostart::is_enabled();
        let mut this = Self {
            next_id: entries.len() as u64,
            draft,
            entries,
            selected: 0,
            url,
            key,
            model,
            interval,
            language,
            recorder: cx.focus_handle().tab_stop(true),
            held_modifier: None,
            released: None,
            auto_start,
            auto_start_saved: auto_start,
            status: notice.map(|text| Status {
                text: text.into(),
                error: true,
            }),
            testing: None,
            updating: None,
            _subscriptions: subscriptions,
        };
        this.select(0, window, cx);
        this
    }

    fn set_status(&mut self, text: impl Into<SharedString>, error: bool, cx: &mut Context<Self>) {
        self.status = Some(Status {
            text: text.into(),
            error,
        });
        cx.notify();
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = index.min(self.entries.len() - 1);
        let profile = self.entries[self.selected].profile.clone();
        for (field, value) in [
            (&self.url, profile.api_base_url),
            (&self.key, profile.api_key),
            (&self.model, profile.model),
        ] {
            field.update(cx, |field, cx| field.set_value(value, window, cx));
        }
        cx.notify();
    }

    fn store_fields(&mut self, cx: &mut Context<Self>) {
        let read = |field: &Entity<InputState>| field.read(cx).value().trim().to_owned();
        let profile = Profile {
            api_base_url: read(&self.url),
            api_key: read(&self.key),
            model: read(&self.model),
        };
        self.entries[self.selected].profile = profile;
        cx.notify();
    }

    fn add_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.entries.push(Entry {
            id: self.next_id,
            profile: Profile::default(),
        });
        self.next_id += 1;
        self.select(self.entries.len() - 1, window, cx);
        self.url.update(cx, |url, cx| url.focus(window, cx));
    }

    fn remove_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.entries.remove(self.selected);
        if self.entries.is_empty() {
            self.add_profile(window, cx);
        } else {
            self.select(self.selected, window, cx);
        }
    }

    fn move_profile(&mut self, up: bool, cx: &mut Context<Self>) {
        let from = self.selected;
        let to = if up {
            from.checked_sub(1)
        } else {
            Some(from + 1).filter(|&i| i < self.entries.len())
        };
        if let Some(to) = to {
            self.entries.swap(from, to);
            self.selected = to;
            cx.notify();
        }
    }

    fn test_profile(&mut self, cx: &mut Context<Self>) {
        let profile = self.entries[self.selected].profile.clone();
        if !profile.is_complete() {
            return self.set_status("请先填写完整的 API 地址、Key 和模型。", true, cx);
        }
        self.set_status("测试中…", false, cx);
        let client = router(cx).client().clone();
        let job = runtime::spawn(async move {
            translate::translate(&client, "你好", Language::English, &profile, &mut |_| {}).await
        });
        self.testing = Some(cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                this.testing = None;
                match result {
                    Ok(translation) => {
                        this.set_status(format!("连接成功：你好 → {}", translation.text), false, cx)
                    }
                    Err(error) => this.set_status(format!("测试失败：{error:#}"), true, cx),
                }
            });
        }));
    }

    fn check_update(&mut self, cx: &mut Context<Self>) {
        if !update::enabled() {
            return self.set_status("调试版本不检查更新。", true, cx);
        }
        if let Some(pending) = cx.global::<UpdateState>().0.clone() {
            if let Err(error) = restart_into(&pending, cx) {
                self.set_status(format!("无法启动新版本：{error}"), true, cx);
            }
            return;
        }
        self.set_status("检查更新中…", false, cx);
        let client = router(cx).client().clone();
        let (progress, mut percents) = mpsc::unbounded();
        let job = runtime::spawn(async move {
            update::check_and_download(&client, move |percent| {
                let _ = progress.unbounded_send(percent);
            })
            .await
        });
        self.updating = Some(cx.spawn(async move |this, cx| {
            while let Some(percent) = percents.next().await {
                let _ = this.update(cx, |this, cx| {
                    this.set_status(format!("下载更新 {percent}%"), false, cx)
                });
            }
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                this.updating = None;
                match result {
                    Ok(None) => this.set_status("已是最新版本。", false, cx),
                    Ok(Some(pending)) => {
                        let text = format!(
                            "{} 已就绪，点击“立即更新”或下次启动时生效。",
                            pending.version
                        );
                        cx.set_global(UpdateState(Some(pending)));
                        this.set_status(text, false, cx);
                    }
                    Err(error) => this.set_status(format!("更新失败：{error:#}"), true, cx),
                }
            });
        }));
    }

    fn record_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.held_modifier = None;
        self.released = None;
        if event.is_held {
            return;
        }
        if let Some((modifiers, key)) = input::pressed_chord() {
            match Trigger::chord(modifiers, key) {
                Some(trigger) => {
                    self.draft.hot_key = trigger;
                    self.status = None;
                }
                None => self.set_status(
                    "快捷键需要包含 Ctrl / Alt / Shift / Win，或使用 F1–F24。",
                    true,
                    cx,
                ),
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// A lone modifier pressed again soon after its release records a double tap.
    fn record_modifiers(
        &mut self,
        event: &ModifiersChangedEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let m = event.modifiers;
        let single = match (m.control, m.alt, m.shift, m.platform) {
            (true, false, false, false) => Some(Modifier::Ctrl),
            (false, true, false, false) => Some(Modifier::Alt),
            (false, false, true, false) => Some(Modifier::Shift),
            _ => None,
        };
        let interval = self.interval_ms(cx);
        match single {
            Some(pressed) => {
                if let Some((released, at)) = self.released.take()
                    && released == pressed
                    && at.elapsed().as_millis() <= u128::from(interval)
                {
                    self.draft.hot_key = Trigger::DoubleTap(pressed);
                    self.status = None;
                    cx.notify();
                }
                self.held_modifier = Some(pressed);
            }
            None if m.number_of_modifiers() == 0 => {
                self.released = self.held_modifier.take().map(|held| (held, Instant::now()));
            }
            None => {
                self.held_modifier = None;
                self.released = None;
            }
        }
    }

    fn record_mouse(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.click_count >= 2 {
            self.draft.hot_key = Trigger::MiddleDoubleClick;
            self.status = None;
            cx.notify();
        }
    }

    fn interval_ms(&self, cx: &App) -> u32 {
        let typed = self.interval.read(cx).value().trim().parse::<f64>().ok();
        typed
            .map_or(self.draft.double_press_interval_ms, |ms| ms.round() as u32)
            .clamp(*INTERVAL_RANGE.start(), *INTERVAL_RANGE.end())
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut settings = self.draft.clone();
        settings.profiles = self
            .entries
            .iter()
            .map(|entry| entry.profile.clone())
            .filter(|p| !p.is_blank())
            .collect();
        settings.double_press_interval_ms = self.interval_ms(cx);
        if let Some(index) = self.language.read(cx).selected_index(cx) {
            settings.foreign_language = Language::ALL[index.row.min(Language::ALL.len() - 1)];
        }
        // The live trigger is suspended while this window is open, so a failure here
        // means another program holds the combination.
        if let Err(error) =
            trigger::listen(settings.hot_key, settings.double_press_interval_ms, || {})
        {
            return self.set_status(format!("{error:#}"), true, cx);
        }
        if let Err(error) = settings.save() {
            return self.set_status(format!("保存失败：{error}"), true, cx);
        }
        router(cx).set_profiles(&settings.profiles);
        cx.set_global(Config(settings));
        if self.auto_start != self.auto_start_saved {
            if let Err(error) = autostart::set_enabled(self.auto_start) {
                return self.set_status(format!("开机自启动设置失败：{error:#}"), true, cx);
            }
            self.auto_start_saved = self.auto_start;
        }
        window.remove_window();
    }

    fn render_profiles(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let rows = self.entries.iter().enumerate().map(|(index, entry)| {
            let profile = &entry.profile;
            let label = match (index, profile.is_complete() || profile.is_blank()) {
                (0, true) => format!("{}（首选）", profile.display_name()),
                (_, true) => profile.display_name(),
                (_, false) => format!("{}（未填完整，不会使用）", profile.display_name()),
            };
            h_flex()
                .id(("profile", entry.id))
                .px_3()
                .py_1()
                .text_sm()
                .cursor_pointer()
                .when(index == self.selected, |row| row.bg(theme.list_active))
                .hover(|row| row.bg(theme.list_hover))
                .child(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.select(index, window, cx)
                }))
        });
        let last = self.entries.len() - 1;
        let tool = |id: &'static str, icon: IconName, tip: &'static str| {
            Button::new(id).ghost().xsmall().icon(icon).tooltip(tip)
        };
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .items_start()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .py_1()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .children(rows),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(tool("add", IconName::Plus, "添加接口").on_click(
                                cx.listener(|this, _, window, cx| this.add_profile(window, cx)),
                            ))
                            .child(tool("remove", IconName::Minus, "删除接口").on_click(
                                cx.listener(|this, _, window, cx| this.remove_profile(window, cx)),
                            ))
                            .child(
                                tool("up", IconName::ChevronUp, "上移（优先使用）")
                                    .disabled(self.selected == 0)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.move_profile(true, cx)),
                                    ),
                            )
                            .child(
                                tool("down", IconName::ChevronDown, "下移")
                                    .disabled(self.selected == last)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.move_profile(false, cx)),
                                    ),
                            ),
                    ),
            )
            .child(field_row(
                "API 地址",
                Input::new(&self.url).into_any_element(),
            ))
            .child(field_row(
                "API Key",
                Input::new(&self.key).mask_toggle().into_any_element(),
            ))
            .child(field_row(
                "模型",
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.model)))
                    .child(
                        Button::new("test")
                            .outline()
                            .label("测试")
                            .loading(self.testing.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.test_profile(cx))),
                    )
                    .into_any_element(),
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("支持 OpenAI 兼容接口；按列表顺序使用，失败时自动切换到下一个。"),
            )
    }

    fn render_trigger(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let recording = self.recorder.is_focused(window);
        let recorder = div()
            .id("recorder")
            .track_focus(&self.recorder)
            .flex_1()
            .h_8()
            .px_3()
            .flex()
            .items_center()
            .rounded(theme.radius)
            .border_1()
            .border_color(if recording { theme.ring } else { theme.input })
            .cursor_pointer()
            .on_key_down(cx.listener(Self::record_key))
            .on_modifiers_changed(cx.listener(Self::record_modifiers))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::record_mouse))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    window.focus(&this.recorder, cx)
                }),
            )
            .child(self.draft.hot_key.label())
            .when(recording, |field| {
                field.child(
                    div()
                        .ml_auto()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("按下新的快捷键"),
                )
            });
        v_flex()
            .gap_2()
            .child(field_row("唤出", recorder.into_any_element()))
            .child(field_row(
                "连按间隔",
                h_flex()
                    .gap_2()
                    .child(div().w(px(140.)).child(NumberInput::new(&self.interval)))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("毫秒"),
                    )
                    .into_any_element(),
            ))
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                "点击上方输入框后按下组合键；也可快速连按两下 Ctrl / Alt / Shift，或双击鼠标中键。",
            ))
    }

    fn render_options(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let language = h_flex()
            .gap_2()
            .child(div().w(px(160.)).child(Select::new(&self.language)))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("中文译为此语言，其他语言译为中文"),
            );
        let mut column = v_flex()
            .gap_3()
            .child(field_row("互译语言", language.into_any_element()));
        for (index, (id, label, checked)) in toggles(&mut self.draft).into_iter().enumerate() {
            column = column.child(Switch::new(id).label(label).checked(*checked).on_change(
                cx.listener(move |this, checked: &bool, _, cx| {
                    *toggles(&mut this.draft)[index].2 = *checked;
                    cx.notify();
                }),
            ));
        }
        column.child(
            Switch::new("autostart")
                .label("开机自启动")
                .checked(self.auto_start)
                .on_change(cx.listener(|this, checked: &bool, _, cx| {
                    this.auto_start = *checked;
                    cx.notify();
                })),
        )
    }

    fn render_about(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let pending = cx.global::<UpdateState>().0.is_some();
        h_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .child(format!("当前版本 {}", update::Version::current())),
            )
            .child(
                Button::new("update")
                    .outline()
                    .small()
                    .label(if pending {
                        "立即更新"
                    } else {
                        "检查更新"
                    })
                    .loading(self.updating.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.check_update(cx))),
            )
    }
}

fn section(title: &'static str, body: impl IntoElement, cx: &Context<Preferences>) -> AnyElement {
    v_flex()
        .gap_3()
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(cx.theme().muted_foreground)
                .child(title),
        )
        .child(body)
        .into_any_element()
}

fn field_row(label: &'static str, field: AnyElement) -> impl IntoElement {
    h_flex()
        .gap_3()
        .child(div().w(px(72.)).flex_none().text_sm().child(label))
        .child(div().flex_1().child(field))
}

impl Render for Preferences {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let profiles = self.render_profiles(cx).into_any_element();
        let trigger = self.render_trigger(window, cx).into_any_element();
        let options = self.render_options(cx).into_any_element();
        let about = self.render_about(cx).into_any_element();
        let theme = cx.theme();
        let status = self.status.as_ref().map(|status| {
            div()
                .flex_1()
                .text_xs()
                .text_color(if status.error {
                    theme.danger
                } else {
                    theme.muted_foreground
                })
                .child(status.text.clone())
        });

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                v_flex().flex_1().min_h_0().overflow_y_scrollbar().child(
                    v_flex()
                        .p_5()
                        .gap_6()
                        .child(section("翻译接口", profiles, cx))
                        .child(section("快捷键", trigger, cx))
                        .child(section("选项", options, cx))
                        .child(section("关于", about, cx)),
                ),
            )
            .child(
                h_flex()
                    .px_5()
                    .py_3()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(status.unwrap_or_else(|| div().flex_1()))
                    .child(
                        Button::new("cancel")
                            .label("取消")
                            .on_click(|_, window, _| window.remove_window()),
                    )
                    .child(
                        Button::new("save")
                            .primary()
                            .label("保存")
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}
