// The user's profile from Helium or Chrome in Filo's browser (crates/filo-local browser_import.rs reads and keeps it):
// the import panel the browser window shows in the page's place (its toolbar's download button), the address bar's
// suggestions from the bookmarks and history brought in, and the saved logins the window offers on a page that asks
// for one. Logins are read from the Keychain once, off the main thread, when a page first needs them; agents never get
// them, only the page the user fills them into with a click.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use filo_local::browser_import::{self, Brought, Login, Place, Profile, Source};
use filo_local::secrets::Secrets;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::theme::{ActiveTheme as _, Typography as _, space};
use crate::ui;

/// The address bar shows at most this many suggestions.
pub const SUGGESTIONS: usize = 8;

enum Logins {
    Unread,
    Reading,
    Read(Rc<Vec<Login>>),
}

thread_local! {
    static PLACES: RefCell<Option<Rc<Vec<Place>>>> = const { RefCell::new(None) };
    static LOGINS: RefCell<Logins> = const { RefCell::new(Logins::Unread) };
}

fn local_dir() -> PathBuf {
    super::local_dir()
}

/// Where the logins are kept: Filo's Keychain item, or a test run's own folder (never the user's Keychain from a
/// check, `dev` builds).
fn logins_store() -> Secrets {
    if cfg!(feature = "dev") && crate::test_run::own_config() && std::env::var_os("FILO_SECRETS_DIR").is_none() {
        return Secrets::Files(local_dir().join("test-secrets"));
    }
    browser_import::secrets()
}

/// The places what's typed matches, best first.
pub fn suggest(typed: &str) -> Vec<Place> {
    let places = PLACES.with(|places| {
        places.borrow_mut().get_or_insert_with(|| Rc::new(browser_import::load_places(&local_dir()))).clone()
    });
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_secs() as i64);
    browser_import::suggest(&places, typed, now, SUGGESTIONS).into_iter().cloned().collect()
}

/// The saved logins, once read; the first call starts reading them (`then` runs when they're in).
pub fn logins(cx: &mut App, then: impl FnOnce(&mut App) + 'static) -> Option<Rc<Vec<Login>>> {
    let state = LOGINS.with(|logins| match &*logins.borrow() {
        Logins::Read(read) => Some(Some(read.clone())),
        Logins::Reading => Some(None),
        Logins::Unread => None,
    });
    if let Some(state) = state {
        return state;
    }
    LOGINS.with(|logins| *logins.borrow_mut() = Logins::Reading);
    let read = cx.background_spawn(async move { browser_import::load_logins(&logins_store()) });
    cx.spawn(async move |cx| {
        let logins = read.await.unwrap_or_else(|error| {
            eprintln!("browser: {error}");
            Vec::new()
        });
        LOGINS.with(|kept| *kept.borrow_mut() = Logins::Read(Rc::new(logins)));
        cx.update(then).ok();
    })
    .detach();
    None
}

/// The saved logins for the page at `url` (none until they're read).
pub fn logins_for(url: &str) -> Vec<Login> {
    LOGINS.with(|logins| match &*logins.borrow() {
        Logins::Read(read) => browser_import::logins_for(read, url).into_iter().cloned().collect(),
        _ => Vec::new(),
    })
}

fn saved_logins() -> Option<usize> {
    LOGINS.with(|logins| match &*logins.borrow() {
        Logins::Read(read) => Some(read.len()),
        _ => None,
    })
}

/// The import panel's close button was pressed.
pub struct Close;

/// A step's state: running, or what it said last.
#[derive(Clone, Default)]
enum Status {
    #[default]
    Idle,
    Running,
    Done(String),
    Failed(String),
}

pub struct ImportPanel {
    source: Option<Source>,
    profiles: Vec<Profile>,
    profile: usize,
    places: Status,
    passwords: Status,
}

impl EventEmitter<Close> for ImportPanel {}

impl ImportPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let source = Source::ALL.into_iter().find(|source| source.installed());
        let mut panel = ImportPanel {
            source: None,
            profiles: Vec::new(),
            profile: 0,
            places: Status::Idle,
            passwords: Status::Idle,
        };
        panel.pick_source(source);
        // The count of logins already saved.
        logins(cx, |_| {});
        panel
    }

    fn pick_source(&mut self, source: Option<Source>) {
        self.source = source;
        self.profiles = source.map(browser_import::profiles).unwrap_or_default();
        self.profile = 0;
        self.places = Status::Idle;
    }

    /// The chosen profile's bookmarks and history (`FILO_OPEN=browser-import-check` calls it too).
    pub fn import_places(&mut self, cx: &mut Context<Self>) {
        let Some(profile) = self.profiles.get(self.profile).cloned() else { return };
        self.places = Status::Running;
        cx.notify();
        let dir = local_dir();
        let read = cx.background_spawn(async move {
            browser_import::import_places(&profile, &dir).map(|brought| (profile, brought))
        });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            if result.is_ok() {
                PLACES.with(|places| *places.borrow_mut() = None);
            }
            this.update(cx, |this, cx| {
                this.places = match result {
                    Ok((profile, Brought { bookmarks, pages })) => Status::Done(format!(
                        "Brought in {} and {} from {}'s {} profile. They show as you type an address.",
                        count(bookmarks, "bookmark"),
                        count(pages, "page of history"),
                        profile.source.name(),
                        profile.name
                    )),
                    Err(error) => Status::Failed(error),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn choose_csv(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = paths.await.ok().and_then(Result::ok).flatten().and_then(|paths| paths.into_iter().next())
            else {
                return;
            };
            this.update(cx, |this, cx| this.import_passwords(path, cx)).ok();
        })
        .detach();
    }

    /// Saves the exported CSV's logins and deletes the file (`FILO_OPEN=browser-import-check` calls it too).
    pub fn import_passwords(&mut self, csv: PathBuf, cx: &mut Context<Self>) {
        self.passwords = Status::Running;
        cx.notify();
        let save = cx.background_spawn(async move {
            let store = logins_store();
            let count = browser_import::import_passwords(&csv, &store)?;
            Ok::<_, String>((count, browser_import::load_logins(&store)?))
        });
        cx.spawn(async move |this, cx| {
            let result = save.await;
            this.update(cx, |this, cx| {
                this.passwords = match result {
                    Ok((saved, all)) => {
                        LOGINS.with(|logins| *logins.borrow_mut() = Logins::Read(Rc::new(all)));
                        Status::Done(format!("Saved {} in Filo, and deleted the file.", count(saved, "login")))
                    }
                    Err(error) => Status::Failed(error),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn forget_logins(&mut self, cx: &mut Context<Self>) {
        match browser_import::forget_logins(&logins_store()) {
            Ok(()) => {
                LOGINS.with(|logins| *logins.borrow_mut() = Logins::Read(Rc::new(Vec::new())));
                self.passwords = Status::Done("Removed the saved logins.".into());
            }
            Err(error) => self.passwords = Status::Failed(error),
        }
        cx.notify();
    }

    /// What the two steps said last ("running" while one runs), for `FILO_OPEN=browser-import-check`.
    #[cfg(feature = "dev")]
    pub fn status(&self) -> (String, String) {
        let text = |status: &Status| match status {
            Status::Idle => String::new(),
            Status::Running => "running".into(),
            Status::Done(text) => text.clone(),
            Status::Failed(error) => format!("failed: {error}"),
        };
        (text(&self.places), text(&self.passwords))
    }

    fn status_line(status: &Status, cx: &App) -> Option<gpui::AnyElement> {
        let colors = *cx.colors();
        let (text, color) = match status {
            Status::Idle | Status::Running => return None,
            Status::Done(text) => (text.clone(), colors.success),
            Status::Failed(error) => (error.clone(), colors.danger),
        };
        Some(div().footnote().text_color(color).child(text).into_any_element())
    }
}

fn count(n: usize, what: &str) -> String {
    match (n, what.split_once(' ')) {
        (1, _) => format!("1 {what}"),
        (_, Some((first, rest))) => format!("{} {first}s {rest}", thousands(n)),
        _ => format!("{} {what}s", thousands(n)),
    }
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

impl Render for ImportPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let installed: Vec<Source> = Source::ALL.into_iter().filter(|source| source.installed()).collect();
        let name = self.source.map_or("the browser", Source::name);
        let mut places: Vec<gpui::AnyElement> = Vec::new();
        if installed.is_empty() {
            places.push(
                div()
                    .callout()
                    .text_color(colors.text_secondary)
                    .child("Neither Helium nor Chrome is on this Mac.")
                    .into_any_element(),
            );
        } else {
            if installed.len() > 1 {
                let selected = self.source.unwrap_or(installed[0]);
                let options =
                    installed.iter().map(|source| (*source, SharedString::from(source.name()), None)).collect();
                let this = cx.entity().downgrade();
                places.push(
                    div()
                        .w(px(240.))
                        .child(ui::segmented(
                            "browser-import-source",
                            options,
                            &selected,
                            move |source, _, cx| {
                                this.update(cx, |this, cx| {
                                    this.pick_source(Some(source));
                                    cx.notify();
                                })
                                .ok();
                            },
                            cx,
                        ))
                        .into_any_element(),
                );
            }
            let rows: Vec<gpui::AnyElement> = self
                .profiles
                .iter()
                .enumerate()
                .map(|(index, profile)| {
                    let chosen = index == self.profile;
                    ui::ListRow::new(
                        SharedString::from(format!("browser-import-profile-{index}")),
                        profile.name.clone(),
                    )
                    .subtitle_opt(Some(profile.email.clone()).filter(|email| !email.is_empty()))
                    .icon("user")
                    .when(chosen, |row| row.trailing(ui::icon_sized("check", 16., colors.accent)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.profile = index;
                        this.places = Status::Idle;
                        cx.notify();
                    }))
                    .into_any_element()
                })
                .collect();
            if rows.is_empty() {
                places.push(
                    div()
                        .callout()
                        .text_color(colors.text_secondary)
                        .child(format!("{name} has no profiles here yet."))
                        .into_any_element(),
                );
            } else {
                places.push(ui::section(Some("Profile"), None, rows, cx).into_any_element());
            }
            places.push(
                div()
                    .flex()
                    .child(
                        ui::Button::primary("browser-import-places", "Import bookmarks and history")
                            .small()
                            .disabled(self.profiles.is_empty())
                            .loading(matches!(self.places, Status::Running))
                            .on_click(cx.listener(|this, _, _, cx| this.import_places(cx))),
                    )
                    .into_any_element(),
            );
            places.extend(Self::status_line(&self.places, cx));
        }
        let saved = saved_logins().unwrap_or(0);
        let step = |n: &str, text: String| {
            div()
                .flex()
                .gap(px(space::SM))
                .child(div().flex_none().w(px(16.)).callout().text_color(colors.text_tertiary).child(n.to_string()))
                .child(div().flex_1().min_w_0().callout().child(text))
        };
        let passwords = div()
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .child(step("1.", format!("In {name}, open chrome://password-manager/settings and choose Export passwords. It saves a .csv file.")))
            .child(step("2.", "Choose that file here. Filo saves its logins in its own Keychain item and deletes the file, which has your passwords in plain text.".into()))
            .child(
                div()
                    .flex()
                    .gap(px(space::SM))
                    .child(
                        ui::Button::secondary("browser-import-csv", "Choose exported file…")
                            .small()
                            .icon("file")
                            .loading(matches!(self.passwords, Status::Running))
                            .on_click(cx.listener(|this, _, window, cx| this.choose_csv(window, cx))),
                    )
                    .when(saved > 0, |row| row.child(ui::Button::ghost("browser-import-forget", "Remove saved logins").small().on_click(cx.listener(|this, _, _, cx| this.forget_logins(cx))))),
            )
            .children(Self::status_line(&self.passwords, cx))
            .when(saved > 0, |this| this.child(div().footnote().text_color(colors.text_secondary).child(format!("{} saved. Filo offers one when its site's login page opens here.", count(saved, "login")))));
        let heading = |text: &'static str| div().headline().child(text);
        div().id("browser-import").size_full().overflow_y_scroll().child(
            div()
                .w(px(560.))
                .max_w_full()
                .mx_auto()
                .py(px(space::XXXL))
                .px(px(space::XL))
                .flex()
                .flex_col()
                .gap(px(space::LG))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .child(ui::icon_sized("download", 20., colors.text_secondary))
                        .child(div().flex_1().title3().child("Bring in from another browser"))
                        .child(ui::IconButton::new("browser-import-close", "x").small().tooltip("Close").on_click(cx.listener(|_, _, _, cx| cx.emit(Close)))),
                )
                .child(div().callout().text_color(colors.text_secondary).child(
                    "Bookmarks and history are read from Helium's or Chrome's own files, and nothing there changes. Passwords come from a file you export from it.",
                ))
                .child(div().flex().flex_col().gap(px(space::MD)).child(heading("Bookmarks and history")).children(places))
                .child(div().h(px(1.)).bg(colors.border))
                .child(div().flex().flex_col().gap(px(space::MD)).child(heading("Passwords")).child(passwords))
                .child(div().footnote().text_color(colors.text_tertiary).child("Filo never reads Helium's or Chrome's own password or cookie stores.")),
        )
    }
}
