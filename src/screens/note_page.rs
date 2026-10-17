// The note page in the app's native web view (web/note.html, web/note-editor.ts), shared by the note editor (note.rs)
// and the skill editor (skill_editor.rs): it builds the page, holds scripts until the page has loaded, follows the
// app's theme and text size, and hides the page under pushed pages and dialogs (a native view draws above everything).

use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine as _;
use futures::StreamExt as _;
use gpui::{App, AppContext as _, Context, Entity, Task, Window};
use gpui_component::WindowExt as _;
use gpui_component::webview::WebView;
use gpui_component::wry;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::theme::{self, ActiveTheme as _};

const PAGE: &str = include_str!("../../web/note.html");
const PAGE_SCRIPT: &str = include_str!("../../assets/web/note-editor.js");

/// A message from the page (web/note-editor.ts `Message`).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PageMessage {
    Title {
        title: String,
    },
    Description {
        description: String,
    },
    Body {
        markdown: String,
    },
    Blur,
    Edit,
    Task {
        line: usize,
    },
    Link {
        url: String,
    },
    /// ⌘S.
    Save,
    Ready,
    Painted,
    #[serde(rename = "filo:nav")]
    Nav {
        dir: String,
    },
    #[serde(other)]
    Other,
}

/// Finds the page in the view that holds it.
pub type PageOf<V> = fn(&mut V) -> Option<&mut NotePage>;

pub struct NotePage {
    web: Entity<WebView>,
    /// The theme last sent to the page, so a theme change is sent once.
    theme_css: String,
    /// False under a pushed page or another tab; the page hides then.
    page_visible: bool,
    dialog_open: bool,
    /// Scripts wait here until the page has loaded (`ready`).
    ready: bool,
    /// The page has drawn; until then whatever is under it shows through.
    painted: bool,
    queued: Vec<String>,
    _ipc: Task<()>,
}

impl NotePage {
    /// Builds the page showing `show` (`FiloNote.show`'s argument). The page's messages go to `on_message`, apart from
    /// `ready` and `painted`, which the page takes itself.
    pub fn build<V: 'static>(
        show: &Value,
        max_title: usize,
        window: &mut Window,
        cx: &mut Context<V>,
        page_of: PageOf<V>,
        on_message: fn(&mut V, PageMessage, &mut Window, &mut Context<V>),
    ) -> Result<NotePage, String> {
        let theme_css = theme_css(cx);
        let html = PAGE
            .replace("{{FONTS}}", font_faces())
            .replace("{{THEME}}", &theme_css)
            .replace("{{MAX_TITLE}}", &max_title.to_string())
            .replace("{{SHOW}}", &show.to_string().replace("</", "<\\/"))
            .replace("{{SCRIPT}}", &PAGE_SCRIPT.replace("</script", "<\\/script"));
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
        let webview = wry::WebViewBuilder::new()
            .with_html(html)
            // No white flash while the page loads: the app's background shows through.
            .with_transparent(true)
            .with_initialization_script(super::artifact::NAV_BRIDGE)
            // Links open in the browser (the page asks), never inside the page.
            .with_navigation_handler(|url| url.starts_with("about:") || url.starts_with("data:"))
            .with_ipc_handler(move |request: wry::http::Request<String>| {
                let _ = tx.unbounded_send(request.body().clone());
            })
            .build_as_child(&*window)
            .map_err(|error| error.to_string())?;
        let web = cx.new(|cx| WebView::new(webview, window, cx));
        let _ipc = cx.spawn_in(window, async move |this, cx| {
            while let Some(message) = rx.next().await {
                let delivered = this.update_in(cx, |this, window, cx| {
                    let Ok(message) = serde_json::from_str::<PageMessage>(&message) else { return };
                    match message {
                        PageMessage::Ready => {
                            if let Some(page) = page_of(this) {
                                page.ready = true;
                                for script in std::mem::take(&mut page.queued) {
                                    page.run_script(script, cx);
                                }
                            }
                        }
                        PageMessage::Painted => {
                            if let Some(page) = page_of(this) {
                                page.painted = true;
                            }
                            cx.notify();
                        }
                        message => on_message(this, message, window, cx),
                    }
                });
                if delivered.is_err() {
                    break;
                }
            }
        });
        Ok(NotePage {
            web,
            theme_css,
            page_visible: true,
            dialog_open: false,
            ready: false,
            painted: false,
            queued: Vec::new(),
            _ipc,
        })
    }

    pub fn web(&self) -> &Entity<WebView> {
        &self.web
    }

    pub fn painted(&self) -> bool {
        self.painted
    }

    pub fn run_script(&mut self, script: String, cx: &App) {
        if !self.ready {
            self.queued.push(script);
            return;
        }
        let _ = self.web.read(cx).raw().evaluate_script(&script);
    }

    /// Shows `args` (`FiloNote.show`'s argument) in place of what's there.
    pub fn show(&mut self, args: &Value, cx: &App) {
        self.run_script(format!("FiloNote.show({args})"), cx);
    }

    /// Sends the app's theme and text size to the page when they changed (call while rendering).
    pub fn follow_theme(&mut self, cx: &App) {
        let css = theme_css(cx);
        if css != self.theme_css {
            self.run_script(format!("FiloNote.setTheme({})", json!(css)), cx);
            self.theme_css = css;
        }
    }

    /// Hidden under a pushed page or another tab (the shell calls this).
    pub fn set_page_visible(&mut self, visible: bool, cx: &mut App) {
        if self.page_visible != visible {
            self.page_visible = visible;
            self.sync(cx);
        }
    }

    fn sync(&self, cx: &mut App) {
        let show = self.page_visible && !self.dialog_open;
        self.web.update(cx, |web, _| match (show, web.visible()) {
            (true, false) => web.show(),
            (false, true) => web.hide(),
            _ => {}
        });
    }

    /// Hides the page while the dialog just opened is up: the dialog draws under the native web view.
    pub fn hide_for_dialog<V: 'static>(&mut self, window: &mut Window, cx: &mut Context<V>, page_of: PageOf<V>) {
        self.dialog_open = true;
        self.sync(cx);
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let open = this.update_in(cx, |_, window, cx| window.has_active_dialog(cx)).unwrap_or(false);
                if !open {
                    break;
                }
            }
            this.update(cx, |this, cx| {
                if let Some(page) = page_of(this) {
                    page.dialog_open = false;
                    page.sync(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Runs `script` in the page and hands back its value as JSON (scripted checks).
    #[cfg(feature = "dev")]
    pub fn evaluate(&self, script: &str, cx: &App) -> futures::channel::oneshot::Receiver<String> {
        let (tx, rx) = futures::channel::oneshot::channel();
        let tx = std::sync::Mutex::new(Some(tx));
        let _ = self.web.read(cx).raw().evaluate_script_with_callback(script, move |value| {
            if let Some(tx) = tx.lock().ok().and_then(|mut tx| tx.take()) {
                let _ = tx.send(value);
            }
        });
        rx
    }
}

/// The page's colours and type, as CSS custom properties (web/note.html).
fn theme_css(cx: &App) -> String {
    let colors = *cx.colors();
    let css = |color: gpui::Hsla| {
        let rgba = gpui::Rgba::from(color);
        format!("rgba({:.0},{:.0},{:.0},{:.3})", rgba.r * 255., rgba.g * 255., rgba.b * 255., rgba.a)
    };
    let prefs = &cx.theme().prefs;
    let scale = theme::text_scale();
    format!(
        "--bg:{};--text:{};--text2:{};--text3:{};--link:{};--border:{};--code-bg:{};--selection:{};--accent:{};--raised:{};--hover:{};\
         --font:'{}';--mono:'{}';--scale:{scale:.4};--size:{:.1}px;--line:{:.0}px;--title-size:{:.1}px;--title-line:{:.0}px;",
        css(colors.background),
        css(colors.text),
        css(colors.text_secondary),
        css(colors.text_tertiary),
        css(colors.link),
        css(colors.border),
        css(colors.surface_strong),
        css(colors.accent.opacity(0.3)),
        css(colors.accent),
        css(colors.raised),
        css(colors.selected_fill),
        prefs.font_family.replace('\'', ""),
        prefs.mono_family.replace('\'', ""),
        15. * scale,
        (23. * scale).round(),
        22. * scale,
        (29. * scale).round(),
    )
}

/// The app's Atkinson Hyperlegible Next, for the page (the web view can't see the app's own fonts).
fn font_faces() -> &'static str {
    static FACES: OnceLock<String> = OnceLock::new();
    FACES.get_or_init(|| {
        // The app's embedded copies (assets.rs), not a second `include_bytes!` of each.
        let face = |file: &str, weight: u16, style: &str| {
            let bytes = crate::assets::Embedded::get(&format!("fonts/{file}")).map(|file| file.data).unwrap_or_default();
            format!(
                "@font-face{{font-family:'Atkinson Hyperlegible Next';font-weight:{weight};font-style:{style};src:url(data:font/ttf;base64,{}) format('truetype');}}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        };
        [
            face("AtkinsonHyperlegibleNext_400Regular.ttf", 400, "normal"),
            face("AtkinsonHyperlegibleNext_400Regular_Italic.ttf", 400, "italic"),
            face("AtkinsonHyperlegibleNext_600SemiBold.ttf", 600, "normal"),
            face("AtkinsonHyperlegibleNext_700Bold.ttf", 700, "normal"),
        ]
        .join("")
    })
}
