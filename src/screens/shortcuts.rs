// Keyboard shortcuts, and Filo outside its window (desktop only): the shortcuts that work in any app
// (companion::SHORTCUTS), changed by clicking one and pressing the new keys, and where voice held from another app goes
// (companion::VoiceTo, voice_route.rs); the menu bar item and the pet, on or off; and the shortcuts inside Filo, listed.

use gpui::{
    Context, Corner, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, ModifiersChangedEvent,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    div, prelude::FluentBuilder as _, px,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button as MenuButton, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::switch::Switch;
use serde_json::json;

use crate::backend;
use crate::companion::{self, SHORTCUTS, SettingsGlobal, VoiceTo};
use crate::hotkeys::{self, Keys};
use crate::screens::agent_picker;
use crate::screens::agents::{AgentRow, GroupRow, group_title};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, ListRow};

/// The shortcuts inside Filo (chrome.rs, screens/chat), as (what, keys). The tabs' go up to as many as the sidebar
/// shows (`nav::sidebar`: fewer with Feed or Notes off).
const IN_FILO: [(&str, &str); 10] = [
    ("Search agents and messages", "⌘P"),
    ("Go to a tab, in the sidebar's order", "⌘1 – ⌘7"),
    ("Back, Forward", "⌘[  ⌘]"),
    ("Hold to talk into the text box with the cursor", "fn ⌃"),
    ("Cancel the recording", "⎋"),
    ("Send, new line", "↩  ⇧↩"),
    ("Zoom an open picture in, out, to fit", "⌘+  ⌘-  ⌘0"),
    ("Settings", "⌘,"),
    ("Keyboard shortcuts", "⌘/"),
    ("Quit Filo", "⌘Q twice"),
];

pub struct ShortcutsScreen {
    focus: FocusHandle,
    /// The shortcut waiting for its new keys.
    recording: Option<&'static str>,
    /// What went wrong setting a shortcut, by id.
    problem: Option<(&'static str, String)>,
    /// Every modifier held together while recording, for a shortcut of modifiers alone (⌥⌘): set when all are let go.
    chord: Option<Keys>,
    /// The agents voice from other apps can go to (`agents:list`).
    agents: Vec<AgentRow>,
    /// The group's name as shown ("Group" until it's given one).
    group: String,
    _settings: Subscription,
    _agents: Task<()>,
    _group: Task<()>,
}

impl ShortcutsScreen {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The pet can be switched from the menu bar or its shortcut while this is open.
        let _settings = cx.observe_global::<SettingsGlobal>(|_, cx| cx.notify());
        let _agents = backend::subscribe(
            cx,
            "agents:list",
            json!({}),
            |this: &mut Self, result: Result<Vec<AgentRow>, String>, cx| {
                if let Ok(agents) = result {
                    this.agents = agents;
                    cx.notify();
                }
            },
        );
        let _group = backend::subscribe(
            cx,
            "agents:group",
            json!({}),
            |this: &mut Self, result: Result<Option<GroupRow>, String>, cx| {
                if let Ok(group) = result {
                    this.group = group_title(group.as_ref().and_then(|group| group.name.as_deref()));
                    cx.notify();
                }
            },
        );
        ShortcutsScreen {
            focus: cx.focus_handle(),
            recording: None,
            problem: None,
            chord: None,
            agents: Vec::new(),
            group: group_title(None),
            _settings,
            _agents,
            _group,
        }
    }

    /// Where voice held from another app goes: the main agent, the open chat or an agent, in a menu.
    fn voice_to_row(&self, choice: &VoiceTo, cx: &gpui::App) -> gpui::AnyElement {
        let colors = cx.colors();
        let main = self.agents.iter().find(|agent| agent.main);
        let main_label = main.map_or_else(|| "Main agent".to_string(), |agent| format!("Main agent ({})", agent.name));
        // A picked agent that's gone is the main agent again.
        let picked = match choice {
            VoiceTo::Agent(id) => self.agents.iter().find(|agent| &agent.id == id),
            _ => None,
        };
        let (shown, label) = match (choice, picked) {
            (_, Some(agent)) => (Some(agent.clone()), agent.name.clone()),
            (VoiceTo::OpenChat, _) => (None, "The open chat".to_string()),
            _ => (main.cloned(), "Main agent".to_string()),
        };
        let current = picked.map(|agent| agent.id.clone());
        let on_main = picked.is_none() && *choice != VoiceTo::OpenChat;
        let open_chat = *choice == VoiceTo::OpenChat;
        // One of the user's own agents, for the example (the group with none).
        let example = self
            .agents
            .iter()
            .find(|agent| !agent.main)
            .or(main)
            .map_or_else(|| self.group.to_string(), |agent| agent.name.clone());
        let agents = self.agents.clone();
        let pick = |choice: VoiceTo, cx: &mut gpui::App| {
            companion::update_settings(cx, move |settings| settings.voice_to = choice)
        };
        let button = MenuButton::new("shortcuts-voice-to")
            .ghost()
            .small()
            .children(shown.map(|agent| agent.tile(18., cx)))
            .child(div().footnote().semibold().text_color(colors.text).child(label))
            .dropdown_caret(true)
            .dropdown_menu_with_anchor(Corner::TopRight, move |menu, _, _| {
                let menu = menu
                    .item(
                        PopupMenuItem::new(main_label.clone())
                            .checked(on_main)
                            .on_click(move |_, _, cx| pick(VoiceTo::Main, cx)),
                    )
                    .item(
                        PopupMenuItem::new("The open chat")
                            .checked(open_chat)
                            .on_click(move |_, _, cx| pick(VoiceTo::OpenChat, cx)),
                    )
                    .separator();
                agent_picker::menu(menu, &agents, current.as_deref(), false, move |agent, _, cx| {
                    pick(agent.map_or(VoiceTo::Main, VoiceTo::Agent), cx);
                })
            });
        ListRow::new("shortcut-voice-to", "Voice from other apps goes to")
            .subtitle(format!(
                "Start with an agent's name or “{}” (“{example}, …”) to send that message there instead. In Filo's window, voice goes to the open chat.",
                self.group
            ))
            .trailing(button)
            .into_any_element()
    }

    fn record(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.recording == Some(id) {
            self.stop(cx);
            return;
        }
        self.recording = Some(id);
        self.problem = None;
        self.chord = None;
        // The old keys mustn't fire while the new ones are typed.
        hotkeys::unregister_all();
        self.focus.focus(window);
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        hotkeys::register(&companion::settings(cx));
        cx.notify();
    }

    fn set(&mut self, id: &'static str, keys: String, cx: &mut Context<Self>) {
        companion::update_settings(cx, |settings| {
            settings.shortcuts.insert(id.to_string(), keys);
        });
        self.stop(cx);
        if hotkeys::is_taken(id) {
            self.problem = Some((id, "Another app uses these keys. Try others.".into()));
        }
    }

    fn reset(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.problem = None;
        companion::update_settings(cx, |settings| {
            settings.shortcuts.remove(id);
        });
        hotkeys::register(&companion::settings(cx));
        if hotkeys::is_taken(id) {
            self.problem = Some((id, "Another app uses these keys. Try others.".into()));
        }
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(id) = self.recording else { return };
        cx.stop_propagation();
        // A key came with the modifiers: it's not a shortcut of modifiers alone.
        self.chord = None;
        let keys = Keys::from_keystroke(&event.keystroke);
        let bare = !(keys.ctrl || keys.alt || keys.cmd || keys.shift);
        match keys.key.as_str() {
            "escape" if bare => return self.stop(cx),
            "backspace" | "delete" if bare => return self.set(id, String::new(), cx),
            _ => {}
        }
        self.try_set(id, keys, cx);
    }

    /// Modifiers alone: the most held together, once all are let go with no key pressed (⌥⌘).
    fn on_modifiers(&mut self, event: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        let Some(id) = self.recording else { return };
        let modifiers = event.modifiers;
        let held = Keys {
            ctrl: modifiers.control,
            alt: modifiers.alt,
            shift: modifiers.shift,
            cmd: modifiers.platform,
            key: String::new(),
        };
        if held.ctrl || held.alt || held.shift || held.cmd {
            let peak = self.chord.get_or_insert_with(Keys::default);
            peak.ctrl |= held.ctrl;
            peak.alt |= held.alt;
            peak.shift |= held.shift;
            peak.cmd |= held.cmd;
            return;
        }
        let Some(chord) = self.chord.take() else { return };
        if chord.usable() {
            self.try_set(id, chord, cx);
        }
    }

    fn try_set(&mut self, id: &'static str, keys: Keys, cx: &mut Context<Self>) {
        if !keys.usable() {
            self.problem =
                Some((id, "Use ⌘, ⌃ or ⌥ with a letter, number or Space, or two of them alone (⌥⌘).".into()));
            cx.notify();
            return;
        }
        let settings = companion::settings(cx);
        let notation = keys.notation();
        if let Some(other) = SHORTCUTS
            .iter()
            .find(|other| other.id != id && settings.shortcut(other.id).as_deref() == Some(notation.as_str()))
        {
            self.problem = Some((id, format!("Already used for “{}”.", other.label)));
            cx.notify();
            return;
        }
        self.set(id, notation, cx);
    }
}

/// Keys drawn as a key cap.
pub fn keycap(label: impl Into<SharedString>, active: bool, cx: &gpui::App) -> gpui::Div {
    let colors = cx.colors();
    div()
        .h(px(28.))
        .min_w(px(28.))
        .px(px(space::SM + 2.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius::SM))
        .border_1()
        .border_color(if active { colors.accent } else { colors.border })
        .bg(if active { colors.accent_soft } else { colors.surface })
        .footnote()
        .semibold()
        .text_color(if active { colors.on_accent_soft } else { colors.text })
        .child(label.into())
}

impl Render for ShortcutsScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let settings = companion::settings(cx);

        let mut global_rows: Vec<gpui::AnyElement> = SHORTCUTS
            .iter()
            .map(|shortcut| {
                let id = shortcut.id;
                let recording = self.recording == Some(id);
                let current = settings.shortcut(id);
                let changed = current.as_deref() != Some(shortcut.default);
                let problem = self.problem.as_ref().filter(|(problem_id, _)| *problem_id == id).map(|(_, problem)| problem.clone());
                let subtitle = match (&problem, recording) {
                    (Some(problem), _) => problem.clone(),
                    (None, true) => "Press the new keys (or two modifiers alone, like ⌥⌘) · Esc keeps the old ones · Delete turns it off".to_string(),
                    (None, false) => shortcut.detail.to_string(),
                };
                let cap = if recording { "Type keys…".to_string() } else { hotkeys::label(current.as_deref()) };
                ListRow::new(SharedString::from(format!("shortcut-{id}")), shortcut.label)
                    .subtitle(subtitle)
                    .trailing(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::SM))
                            .when(changed && !recording, |this| {
                                this.child(
                                    div()
                                        .id(SharedString::from(format!("shortcut-reset-{id}")))
                                        .footnote()
                                        .text_color(colors.text_tertiary)
                                        .cursor_pointer()
                                        .hover(|style| style.text_color(colors.text_secondary))
                                        .child("Reset")
                                        .on_click(cx.listener(move |this, _, _, cx| this.reset(id, cx))),
                                )
                            })
                            .child(
                                keycap(cap, recording, cx)
                                    .id(SharedString::from(format!("shortcut-keys-{id}")))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, window, cx| this.record(id, window, cx))),
                            ),
                    )
                    .into_any_element()
            })
            .collect();
        global_rows.push(self.voice_to_row(&settings.voice_to, cx));

        let desktop_rows = vec![
            ListRow::new("shortcuts-menu-bar", "Filo in the menu bar")
                .subtitle("New replies and what's next, a click away.")
                .trailing(Switch::new("shortcuts-menu-bar-switch").checked(settings.menu_bar).on_click(cx.listener(|_, checked: &bool, _, cx| {
                    let on = *checked;
                    companion::update_settings(cx, |settings| settings.menu_bar = on);
                })))
                .into_any_element(),
            ListRow::new("shortcuts-pet", "Filo's pet")
                .subtitle("Sits on your desktop above every app: shows what Filo is doing, pops up replies and reminders, and opens Filo when clicked. Drag it anywhere; right-click for more.")
                .trailing(Switch::new("shortcuts-pet-switch").checked(settings.pet).on_click(cx.listener(|_, checked: &bool, _, cx| {
                    let on = *checked;
                    companion::update_settings(cx, |settings| settings.pet = on);
                })))
                .into_any_element(),
        ];

        let tabs = format!("⌘1 – ⌘{}", crate::nav::sidebar(cx).len());
        let in_filo_rows = IN_FILO
            .iter()
            .enumerate()
            .map(|(index, (what, keys))| {
                let keys = if *keys == "⌘1 – ⌘7" { tabs.as_str() } else { *keys };
                ListRow::new(SharedString::from(format!("shortcut-in-filo-{index}")), *what)
                    .trailing(keycap(keys.to_string(), false, cx))
                    .into_any_element()
            })
            .collect();

        div()
            .id("shortcuts")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| this.on_modifiers(event, cx)))
            .size_full()
            .child(ui::sub_page(
                ui::sub_page_header("shortcuts", "Keyboard shortcuts", vec![], cx),
                div().id("shortcuts-scroll").flex_1().min_h_0().overflow_y_scroll().child(
                    ui::page_column()
                        .pt(px(space::XXL))
                        .pb(px(48.))
                        .flex()
                        .flex_col()
                        .gap(px(space::XXL))
                        .child(ui::section(
                            Some("Anywhere on your Mac"),
                            Some("These work while another app is in front. Click one to change it.".into()),
                            global_rows,
                            cx,
                        ))
                        .child(ui::section(Some("On your desktop"), None, desktop_rows, cx))
                        .child(ui::section(Some("In Filo"), None, in_filo_rows, cx))
                        .child(div().footnote().text_color(colors.text_tertiary).child(
                            "Closing Filo's window keeps it running in the menu bar while the menu bar item or the pet is on; pressing ⌘Q twice quits.",
                        )),
                ),
                cx,
            ))
    }
}
