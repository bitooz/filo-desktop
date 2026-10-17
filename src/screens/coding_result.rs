// A coding round's result (`coding:roundResult`, local_backend/handoff.rs), opened from a chat message's Open proof:
// the round's result.md as the tool reported it, with the round's screenshots where the markdown puts them, as a repo's README shows them, and its recordings as a frame from each with the play
// button and its length. Screenshots and recordings open in the app's viewer (media_viewer.rs), where a recording
// plays and the arrows step through the rest; logs open in Quick Look. Only the round's own uploads
// become pictures (`segments`); the markdown is drawn untrusted, so a picture it points at elsewhere is a link and
// nothing else is fetched.
//
// An agent's full report under its short post in the group (send_message's `report`) opens here too, from the post's
// row (conversation.rs `report_footer`): the same page, headed by the agent, its name and when it posted, and titled by
// the report's leading heading; it has no rounds, files or Share.
//
// Share puts a copy of the round's result, without this Mac's paths, on a page at filo.im/results/<id> that anyone
// with its link can open, and copies the link (local_backend/result_share.rs); the page then says it's shared, with
// Copy link and Unshare, which takes the page down.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

use gpui::{
    AnyElement, Context, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::text::TextView;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

use super::coding::{Proof, proof_row, tool_name, tool_tile};
use crate::backend;
use crate::file_preview;
use crate::format;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button};

/// Like a page's reading column.
pub(crate) const PAGE_WIDTH: f32 = 820.;
/// The widest an inline screenshot is drawn.
const PICTURE_WIDTH: f32 = 640.;
/// The tallest an inline screenshot is drawn.
const PICTURE_HEIGHT: f32 = 720.;
/// Up to this many rounds the switcher shows each one's number; past it, "‹ Round 5 of 9 ›".
const ROUND_NUMBERS: u32 = 6;
/// The switcher's room from the bar's end (which has `space::SM` of its own): it ends as far from the window's right
/// edge as the back arrow starts from the sidebar's card on the left, 23 points.
const SWITCHER_END: f32 = 14.;

/// Where the round is shared (`coding:roundResult`'s `share`).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Shared {
    url: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoundResult {
    title: String,
    tool: String,
    status: String,
    /// The task's status now (`coding:roundResult` sends it as the task's rows and cards have it).
    #[serde(default)]
    task_status: Option<String>,
    round: f64,
    rounds: f64,
    created_at: f64,
    result: String,
    #[serde(default)]
    files: Vec<Proof>,
    #[serde(default)]
    share: Option<Shared>,
    /// How long the round worked, in milliseconds (`coding_handoff::round_worked_ms`); none while it runs.
    #[serde(default)]
    worked_ms: Option<f64>,
}

/// An agent's full report under its short post in the group, as its page shows it (`CodingResultScreen::report`).
#[derive(Clone, Debug, PartialEq)]
pub struct PostReport {
    /// The post's id, for the page's element ids.
    pub id: String,
    /// The agent who posted it (none for its line in the agent's own chat), and its name.
    pub agent: Option<String>,
    pub author: String,
    pub created_at: f64,
    pub report: String,
}

pub struct CodingResultScreen {
    task_id: String,
    /// The page shows an agent's report instead of a round (`report`).
    report: Option<PostReport>,
    /// The round picked in the switcher (or opened), highlighted there while its result is on the way; none: the
    /// latest.
    round: Option<u32>,
    result: Option<Result<Option<RoundResult>, String>>,
    /// Holds the keys, so ← and → switch rounds.
    focus: FocusHandle,
    focused: bool,
    /// Share or Unshare under way.
    sharing: bool,
    _sub: Task<()>,
}

impl CodingResultScreen {
    pub fn new(task_id: String, round: Option<u32>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _sub = Self::subscribe(&task_id, round, cx);
        CodingResultScreen {
            task_id,
            report: None,
            round,
            result: None,
            focus: cx.focus_handle(),
            focused: false,
            sharing: false,
            _sub,
        }
    }

    /// An agent's full report under its post: already in the post, so nothing to fetch.
    pub fn report(report: PostReport, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        CodingResultScreen {
            task_id: String::new(),
            report: Some(report),
            round: None,
            result: None,
            focus: cx.focus_handle(),
            focused: false,
            sharing: false,
            _sub: Task::ready(()),
        }
    }

    fn subscribe(task_id: &str, round: Option<u32>, cx: &mut Context<Self>) -> Task<()> {
        let args = match round {
            Some(round) => json!({ "taskId": task_id, "round": round }),
            None => json!({ "taskId": task_id }),
        };
        backend::subscribe(cx, "coding:roundResult", args, |this: &mut Self, result, cx| {
            this.result = Some(result);
            cx.notify();
        })
    }

    /// Another round of the same task, in place (one subscription at a time).
    fn show_round(&mut self, round: u32, cx: &mut Context<Self>) {
        if self.rounds().is_some_and(|(shown, _)| shown == round) {
            return;
        }
        self.round = Some(round);
        self._sub = Self::subscribe(&self.task_id, Some(round), cx);
        cx.notify();
    }

    /// The round the switcher shows as current, and how many the task has, once a result is here.
    fn rounds(&self) -> Option<(u32, u32)> {
        match &self.result {
            Some(Ok(Some(result))) => Some((self.round.unwrap_or(result.round as u32), result.rounds as u32)),
            _ => None,
        }
    }

    /// Share: asks first, saying what will go (`coding:sharePreview`): who can see it, the files that go and those that
    /// stay, what goes unread, and the keys taken out. Then `send_share`.
    fn share(&mut self, round: u32, window: &mut Window, cx: &mut Context<Self>) {
        if self.sharing {
            return;
        }
        let args = json!({ "taskId": self.task_id, "round": round });
        backend::call_in(
            window,
            cx,
            "coding:sharePreview",
            args,
            move |_: &mut Self, result: Result<Value, String>, window, cx| {
                let preview = match result {
                    Ok(preview) => preview,
                    Err(error) => return ui::toast_error(window, cx, "Couldn’t share it", error),
                };
                let entity = cx.entity().downgrade();
                let keys = preview["hidden"].as_u64().unwrap_or(0) > 0;
                ui::confirm(
                    window,
                    cx,
                    "Share this result?",
                    share_question(&preview),
                    "Share",
                    "Cancel",
                    keys,
                    move |window, cx| {
                        entity.update(cx, |this, cx| this.send_share(round, window, cx)).ok();
                    },
                );
            },
        );
    }

    /// The round's result to its page on filo.im, then the page's link on the clipboard. Shared already, the same link.
    fn send_share(&mut self, round: u32, window: &mut Window, cx: &mut Context<Self>) {
        if self.sharing {
            return;
        }
        self.sharing = true;
        cx.notify();
        let args = json!({ "taskId": self.task_id, "round": round });
        backend::call_in(
            window,
            cx,
            "coding:shareResult",
            args,
            |this: &mut Self, result: Result<Value, String>, window, cx| {
                this.sharing = false;
                match result.map(|value| value["url"].as_str().map(str::to_string)) {
                    Ok(Some(url)) => {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
                        ui::toast(window, cx, "Shared. Link copied");
                    }
                    Ok(None) => ui::toast_error(window, cx, "Couldn’t share it", "No link came back."),
                    Err(error) => ui::toast_error(window, cx, "Couldn’t share it", error),
                }
                cx.notify();
            },
        );
    }

    /// Unshare: the page on filo.im is deleted, so its link opens nothing any more.
    fn unshare(&mut self, round: u32, window: &mut Window, cx: &mut Context<Self>) {
        if self.sharing {
            return;
        }
        self.sharing = true;
        cx.notify();
        let args = json!({ "taskId": self.task_id, "round": round });
        backend::call_in(
            window,
            cx,
            "coding:unshareResult",
            args,
            |this: &mut Self, result: Result<Value, String>, window, cx| {
                this.sharing = false;
                match result {
                    Ok(_) => ui::toast(window, cx, "Unshared. The link doesn’t open it any more"),
                    Err(error) => ui::toast_error(window, cx, "Couldn’t unshare it", error),
                }
                cx.notify();
            },
        );
    }

    /// ← and →: the round before or after, stopping at the first and the last.
    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt || modifiers.function || modifiers.shift {
            return;
        }
        let Some((round, rounds)) = self.rounds().filter(|(_, rounds)| *rounds > 1) else { return };
        let next = match event.keystroke.key.as_str() {
            "left" => round.saturating_sub(1).max(1),
            "right" => (round + 1).min(rounds),
            _ => return,
        };
        cx.stop_propagation();
        self.show_round(next, cx);
    }

    /// The top bar's round switcher (for a task of more than one round): "Round" and each round's number, in a track
    /// like the app's segmented controls, or "‹ Round 5 of 9 ›" when there are too many to list.
    fn round_switcher(&self, round: u32, rounds: u32, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let track =
            div().flex().items_center().p(px(2.)).gap(px(2.)).rounded(px(radius::SM + 2.)).bg(colors.surface_strong);
        let segment = |id: SharedString| {
            div()
                .id(id)
                .flex_none()
                .h(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius::SM))
                .footnote()
        };
        if rounds <= ROUND_NUMBERS {
            let numbers = (1..=rounds).map(|n| {
                let id = format!("coding-result-round-{n}");
                let number = segment(SharedString::from(id.clone()))
                    .min_w(px(26.))
                    .px(px(space::SM))
                    .cursor_pointer()
                    .map(|this| {
                        if n == round {
                            this.bg(colors.raised)
                                .border_1()
                                .border_color(colors.border)
                                .semibold()
                                .text_color(colors.text)
                        } else {
                            this.medium().text_color(colors.text_secondary).hover(|style| style.text_color(colors.text))
                        }
                    })
                    .child(n.to_string())
                    .on_click(cx.listener(move |this, _, _, cx| this.show_round(n, cx)));
                crate::smoke::tracked(id, number)
            });
            return div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .mr(px(SWITCHER_END))
                .child(div().footnote().medium().text_color(colors.text_secondary).child("Round"))
                .child(track.children(numbers))
                .into_any_element();
        }
        let arrow = |id: &'static str, icon: &'static str, to: u32, enabled: bool, cx: &mut Context<Self>| {
            let arrow = segment(id.into())
                .group(id)
                .w(px(24.))
                .map(|this| {
                    if enabled {
                        this.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.show_round(to, cx)))
                    } else {
                        this.opacity(0.42)
                    }
                })
                .child(
                    ui::icon(icon)
                        .size(px(14.))
                        .text_color(colors.text_secondary)
                        .when(enabled, |glyph| glyph.group_hover(id, |style| style.text_color(colors.text))),
                );
            crate::smoke::tracked(id, arrow)
        };
        div()
            .flex_none()
            .mr(px(SWITCHER_END))
            .child(
                track
                    .child(arrow(
                        "coding-result-round-previous",
                        "chevron-left",
                        round.saturating_sub(1).max(1),
                        round > 1,
                        cx,
                    ))
                    .child(
                        div()
                            .px(px(space::XS))
                            .footnote()
                            .semibold()
                            .text_color(colors.text)
                            .child(format!("Round {round} of {rounds}")),
                    )
                    .child(arrow(
                        "coding-result-round-next",
                        "chevron-right",
                        (round + 1).min(rounds),
                        round < rounds,
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn render_result(&self, result: &RoundResult, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let round = result.round as u32;
        // The task's state while it still works, the same as its card and row; after, how this round ended ("Waited" for
        // one that only waited on background work). A round that ended as it should says nothing.
        let (state, label) = super::coding::round_page_state(
            &result.status,
            result.task_status.as_deref(),
            result.round >= result.rounds,
        );
        let status = (state != "completed").then_some(label);
        let meta = div()
            .child(format!("{} ·", tool_name(&result.tool)))
            .when_some(status, |row, label| {
                row.children(super::agent_work::state_mark(state, 14., colors.text_secondary))
                    .child(format!("{label} ·"))
            })
            .child(format::short_date_time(result.created_at))
            .children(result.worked_ms.map(|worked| format!("· {}", format::worked_long(worked))));
        let hero = page_hero(tool_tile(&result.tool, 44., cx).into_any_element(), meta, result.title.clone(), cx)
            // A round still at work has nothing to share yet.
            .when(result.share.is_none() && result.status != "running", |this| {
                let share = Button::secondary("coding-result-share", if self.sharing { "Sharing…" } else { "Share" })
                    .small()
                    .icon("share-2")
                    .loading(self.sharing)
                    .disabled(self.sharing)
                    .on_click(cx.listener(move |this, _, window, cx| this.share(round, window, cx)));
                this.child(crate::smoke::tracked("coding-result-share", share))
            });

        let shared = result.share.as_ref().map(|shared| {
            let unshare = cx.listener(move |this, _, window, cx| this.unshare(round, window, cx));
            shared_bar(
                "coding-result",
                &shared.url,
                "Shared: anyone with the link can open it".into(),
                self.sharing,
                None,
                unshare,
                cx,
            )
        });
        // The page's own title says what the markdown's first heading does.
        self.render_page(&round.to_string(), hero, shared, &strip_title(&result.result), &result.files, window, cx)
    }

    /// An agent's report: its picture, name and when it posted over the report, titled by its leading heading.
    fn render_report(&self, report: &PostReport, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let face = crate::nav::share_targets(cx)
            .into_iter()
            .find(|target| report.agent.as_ref().map_or(target.name == report.author, |agent| target.id == *agent));
        let mark = match face {
            Some(face) => super::agents::picture_on(face.picture, face.background, 44., cx).into_any_element(),
            None => div()
                .size(px(44.))
                .flex_none()
                .rounded(px(radius::FULL))
                .bg(colors.surface_strong)
                .flex()
                .items_center()
                .justify_center()
                .child(ui::icon_sized("file-text", 20., colors.text_secondary))
                .into_any_element(),
        };
        let meta = div().child(format!("{} ·", report.author)).child(format::short_date_time(report.created_at));
        let (title, markdown) =
            leading_heading(&report.report).unwrap_or_else(|| (REPORT.to_string(), report.report.clone()));
        let hero = page_hero(mark, meta, title, cx);
        self.render_page(&format!("report-{}", report.id), hero, None, &markdown, &[], window, cx)
    }

    /// The page a round's result and an agent's report share: `hero` and any `shared` bar over the markdown, with the
    /// round's pictures and recordings where it puts them, in the reading column. `key` tells its markdown's ids apart.
    #[allow(clippy::too_many_arguments)]
    fn render_page(
        &self,
        key: &str,
        hero: gpui::Div,
        shared: Option<AnyElement>,
        markdown: &str,
        files: &[Proof],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = *cx.colors();
        let column = div().flex().flex_col().gap(px(space::LG)).child(hero).children(shared);
        let known: Vec<(String, String)> = files.iter().map(|file| (file.name.clone(), file.kind.clone())).collect();
        let urls: HashMap<String, String> = files.iter().map(|file| (file.name.clone(), file.url.clone())).collect();
        let by_name: HashMap<&str, &Proof> = files.iter().map(|file| (file.name.as_str(), file)).collect();
        let segments = segments(markdown, &known, &urls);
        // The round's pictures and recordings in the order the page shows them: one opens the viewer there, and the
        // arrows step through the rest.
        let shown: Vec<&Proof> = segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Media(name) => {
                    by_name.get(name.as_str()).copied().filter(|file| matches!(file.kind.as_str(), "image" | "video"))
                }
                Segment::Text(_) => None,
            })
            .collect();
        let gallery: Rc<Vec<file_preview::Item>> = Rc::new(shown.iter().map(|file| file.preview_item()).collect());
        let opens = |file: &Proof| {
            let (gallery, at) =
                (gallery.clone(), shown.iter().position(|shown| std::ptr::eq(*shown, file)).unwrap_or(0));
            move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut gpui::App| {
                crate::media_viewer::open(gallery.to_vec(), at, None, window, cx)
            }
        };
        let mut body = div().mt(px(space::SM)).flex().flex_col().gap(px(space::MD));
        for (index, segment) in segments.into_iter().enumerate() {
            body = body.child(match segment {
                Segment::Text(text) => {
                    TextView::markdown(SharedString::from(format!("coding-result-{key}-{index}")), text, window, cx)
                        .style(ui::markdown_page_style().untrusted(true))
                        .selectable(true)
                        .into_any_element()
                }
                Segment::Media(name) => match by_name.get(name.as_str()) {
                    Some(file) if file.kind == "image" => inline_picture(file, index, opens(file), window, cx),
                    Some(file) if file.kind == "video" => {
                        crate::smoke::tracked("coding-result-video", inline_video(file, index, opens(file), cx))
                    }
                    Some(file) => div()
                        .max_w(px(420.))
                        .p(px(space::SM + 2.))
                        .rounded(px(radius::LG))
                        .bg(colors.surface)
                        .border_1()
                        .border_color(colors.border)
                        .child(proof_row(file, cx))
                        .into_any_element(),
                    // `segments` only places the round's own files.
                    None => div().into_any_element(),
                },
            });
        }
        let column = column.child(body);

        div()
            .id("coding-result-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .max_w(px(PAGE_WIDTH))
                    .mx_auto()
                    .px(px(crate::theme::PAGE_PADDING))
                    .pt(px(space::XXL + 4.))
                    .pb(px(space::HUGE))
                    .child(column),
            )
            .into_any_element()
    }
}

/// The page's head: the round's tool or the agent (`mark`), a line saying who and when (`meta`), and the title.
fn page_hero(mark: AnyElement, meta: gpui::Div, title: String, cx: &gpui::App) -> gpui::Div {
    let colors = *cx.colors();
    div().flex().items_center().gap(px(space::LG)).child(mark).child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .child(meta.flex().items_center().gap(px(space::XS)).subhead().semibold().text_color(colors.text_secondary))
            .child(div().title1().bold().child(title)),
    )
}

/// A screenshot in the result, at its own shape up to the column's width; it opens larger in the viewer.
fn inline_picture(
    file: &Proof,
    index: usize,
    open: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    window: &mut Window,
    cx: &mut Context<CodingResultScreen>,
) -> AnyElement {
    let colors = *cx.colors();
    let picture = file.picture();
    let (width, height) = crate::pictures::natural_size_of(&picture, window, cx)
        .filter(|(w, h)| *w > 0. && *h > 0.)
        .unwrap_or((16., 10.));
    let (shown_width, shown_height) = picture_box(width, height);
    div()
        .id(SharedString::from(format!("coding-result-picture-{index}")))
        .relative()
        .flex_none()
        .w(px(shown_width))
        .h(px(shown_height))
        .rounded(px(radius::MD))
        .bg(colors.surface_strong)
        .cursor_pointer()
        .hover(|style| style.opacity(0.9))
        .child(ui::rounded_cover(gpui::ImageSource::Resource(picture), radius::MD, colors.background))
        .on_click(open)
        .into_any_element()
}

/// A recording in the result, like a screenshot: its poster (a frame from it, video_player.rs) at its shape, with
/// the play button and its length over it; it plays in the viewer.
fn inline_video(
    file: &Proof,
    index: usize,
    open: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    cx: &mut Context<CodingResultScreen>,
) -> AnyElement {
    let colors = *cx.colors();
    let poster = crate::video_player::poster(&file.preview_item(), cx);
    let (width, height) =
        poster.as_ref().map(|poster| poster.size).filter(|(w, h)| *w > 0. && *h > 0.).unwrap_or((16., 9.));
    let (shown_width, shown_height) = picture_box(width, height);
    div()
        .id(SharedString::from(format!("coding-result-video-{index}")))
        .relative()
        .flex_none()
        .w(px(shown_width))
        .h(px(shown_height))
        .rounded(px(radius::MD))
        .bg(colors.surface_strong)
        .cursor_pointer()
        .hover(|style| style.opacity(0.9))
        .when_some(poster.as_ref(), |this, poster| {
            this.child(ui::rounded_cover(
                gpui::ImageSource::Resource(gpui::Resource::Path(poster.picture.as_path().into())),
                radius::MD,
                colors.background,
            ))
        })
        .child(crate::video_player::badge(poster.and_then(|poster| poster.duration_ms), cx))
        .on_click(open)
        .into_any_element()
}

/// The box a `width` × `height` picture is drawn in: the column's width at most, and no taller than `PICTURE_HEIGHT`,
/// narrower then, so the whole picture shows.
fn picture_box(width: f32, height: f32) -> (f32, f32) {
    let shown = PICTURE_WIDTH.min(width.max(240.));
    let tall = shown * height / width;
    if tall <= PICTURE_HEIGHT { (shown, tall) } else { (PICTURE_HEIGHT * width / height, PICTURE_HEIGHT) }
}

/// The markdown without its first `# ` heading (the round's title, which the page shows).
/// What Share asks (security review M1), from `coding:sharePreview`: who can see it, the files that go and those that
/// stay, the pictures, recordings and PDFs that go unread, and the keys taken out of the text.
fn share_question(preview: &Value) -> String {
    let names = |key: &str| -> Vec<String> {
        preview[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|file| file.as_str().or_else(|| file["name"].as_str()))
            .map(str::to_string)
            .collect()
    };
    let mut lines = vec!["Anyone with the link can open it, until you unshare it.".to_string()];
    let files = names("files");
    lines.push(if files.is_empty() {
        "Only the report goes.".into()
    } else {
        format!("Goes with the report: {}.", and_list(&files))
    });
    let left: Vec<String> = preview["left"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| Some(format!("{} ({})", file["name"].as_str()?, file["why"].as_str()?)))
        .collect();
    if !left.is_empty() {
        lines.push(format!("Stays on this Mac: {}.", and_list(&left)));
    }
    match preview["hidden"].as_u64().unwrap_or(0) {
        0 => lines.push("Paths are taken out of the text, and so is anything that looks like a key.".into()),
        1 => lines.push(
            "1 thing that looks like a key or a token is taken out of the text. Check the report for others.".into(),
        ),
        hidden => lines.push(format!(
            "{hidden} things that look like keys or tokens are taken out of the text. Check the report for others."
        )),
    }
    let as_is = names("asIs");
    if !as_is.is_empty() {
        let (these, go) = if as_is.len() == 1 { ("It", "goes") } else { ("They", "go") };
        lines.push(format!("{} {go} as {}: nothing reads them. {these} may show something private (a key on screen, a name, an address).", and_list(&as_is), if as_is.len() == 1 { "it is" } else { "they are" }));
    }
    lines.join("\n\n")
}

/// "a", "a and b", "a, b and c".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What a report without a leading heading is called, on its page and its post's row.
const REPORT: &str = "Full report";

/// A report's title: its leading heading (any level), else "Full report".
pub(crate) fn report_title(report: &str) -> String {
    leading_heading(report).map_or_else(|| REPORT.to_string(), |(title, _)| title)
}

/// A report's leading heading, of any level ("## Release for friends"), and the rest of it.
fn leading_heading(markdown: &str) -> Option<(String, String)> {
    let markdown = markdown.trim_start();
    let (first, rest) = markdown.split_once('\n').unwrap_or((markdown, ""));
    let level = first.len() - first.trim_start_matches('#').len();
    let title =
        first[level..].strip_prefix(' ').filter(|_| (1..=6).contains(&level))?.trim().trim_end_matches('#').trim();
    (!title.is_empty()).then(|| (title.to_string(), rest.trim_start_matches('\n').to_string()))
}

fn strip_title(markdown: &str) -> String {
    match markdown.strip_prefix("# ") {
        Some(rest) => {
            rest.split_once('\n').map(|(_, body)| body.trim_start_matches('\n').to_string()).unwrap_or_default()
        }
        None => markdown.to_string(),
    }
}

#[derive(Debug, PartialEq)]
enum Segment {
    Text(String),
    /// One of the round's files, by name.
    Media(String),
}

fn reference() -> &'static Regex {
    static REF: OnceLock<Regex> = OnceLock::new();
    REF.get_or_init(|| {
        Regex::new(r#"(!?)\[([^\]\n]*)\]\(\s*(<[^>\n]*>|[^)\s]+)(?:\s+"[^"\n]*")?\s*\)"#).expect("regex")
    })
}

/// Whether a link target is a file in the round's folder rather than an address.
fn is_local(target: &str) -> bool {
    let target = target.trim().trim_start_matches('<');
    let scheme = target.split_once(':').is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
            && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
    });
    !scheme && !target.starts_with('#')
}

fn ref_name(target: &str) -> String {
    let name = target.trim().trim_start_matches('<').trim_end_matches('>');
    let name = name.strip_prefix("./").unwrap_or(name);
    url::form_urlencoded::parse(format!("x={}", name.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
        .unwrap_or_else(|| name.to_string())
}

/// The result's pieces: markdown, and the round's own pictures
/// and recordings where it names them. Links to its other files point at their uploads; to files it doesn't have,
/// plain words.
fn segments(markdown: &str, files: &[(String, String)], urls: &HashMap<String, String>) -> Vec<Segment> {
    let kinds: HashMap<&str, &str> = files.iter().map(|(name, kind)| (name.as_str(), kind.as_str())).collect();
    let mut out = Vec::new();
    let mut text = String::new();
    let mut last = 0;
    for captures in reference().captures_iter(markdown) {
        let whole = captures.get(0).expect("match");
        let embedded = captures.get(1).is_some_and(|m| m.as_str() == "!");
        let label = captures.get(2).map(|m| m.as_str()).unwrap_or_default();
        let target = captures.get(3).map(|m| m.as_str()).unwrap_or_default();
        text.push_str(&markdown[last..whole.start()]);
        last = whole.end();
        if !is_local(target) {
            text.push_str(whole.as_str());
            continue;
        }
        let name = ref_name(target);
        // Pictures and recordings show in place; any other file embedded (`![log](tests.log)`) shows as its row.
        let kind = kinds.get(name.as_str()).copied();
        if matches!(kind, Some("image" | "video")) || (embedded && kind.is_some()) {
            // A list item holding only the picture leaves its bullet behind; drop it with the picture.
            let trimmed = text.trim_end_matches([' ', '\t']);
            let line_start = trimmed.rfind('\n').map(|at| at + 1).unwrap_or(0);
            let tail = trimmed[line_start..].trim();
            if tail == "-"
                || tail == "*"
                || tail == "+"
                || (tail.ends_with('.') && tail[..tail.len() - 1].chars().all(|c| c.is_ascii_digit()) && tail.len() > 1)
            {
                text.truncate(line_start);
            }
            if !text.trim().is_empty() {
                out.push(Segment::Text(std::mem::take(&mut text)));
            }
            text.clear();
            out.push(Segment::Media(name));
        } else {
            let words = if label.is_empty() { name.clone() } else { label.to_string() };
            match urls.get(&name) {
                Some(url) => text.push_str(&format!("[{words}]({url})")),
                None => text.push_str(&words),
            }
        }
    }
    text.push_str(&markdown[last..]);
    if !text.trim().is_empty() {
        out.push(Segment::Text(text));
    }
    out
}

impl Render for CodingResultScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused {
            self.focused = true;
            self.focus.focus(window);
        }
        if let Some(report) = self.report.clone() {
            let header = ui::sub_page_header("coding-result", "Report", Vec::new(), cx);
            let body = self.render_report(&report, window, cx);
            return ui::sub_page(header, body, cx)
                .track_focus(&self.focus)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)));
        }
        let switcher = self
            .rounds()
            .filter(|(_, rounds)| *rounds > 1)
            .map(|(round, rounds)| self.round_switcher(round, rounds, cx));
        let header = ui::sub_page_header("coding-result", "Result", switcher.into_iter().collect(), cx);
        let body: AnyElement = match self.result.clone() {
            None => ui::page_column().pt(px(space::LG)).child(ui::skeleton_document()).into_any_element(),
            Some(Err(error)) => {
                ui::page_column().pt(px(space::LG)).child(ui::banner(error, ui::Tone::Danger, cx)).into_any_element()
            }
            Some(Ok(None)) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(space::XXL))
                .child(div().title3().bold().child("This result isn’t available."))
                .into_any_element(),
            Some(Ok(Some(result))) => self.render_result(&result, window, cx),
        };
        ui::sub_page(header, body, cx)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
    }
}

/// The line under a shared page's title (a round's result, an artifact): where it is, with Copy link, any `extra`
/// (an artifact's Update) and Unshare. Its parts' ids start with `id` (`coding-result-copy-link`, `artifact-unshare`).
pub(crate) fn shared_bar(
    id: &str,
    url: &str,
    headline: SharedString,
    unsharing: bool,
    extra: Option<AnyElement>,
    on_unshare: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    cx: &gpui::App,
) -> AnyElement {
    let colors = *cx.colors();
    let address = url.trim_start_matches("https://").to_string();
    let copied = url.to_string();
    let copy = Button::secondary(SharedString::from(format!("{id}-copy-link")), "Copy link")
        .small()
        .icon("link")
        .on_click(move |_, window, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(copied.clone()));
            ui::toast(window, cx, "Link copied");
        });
    let unshare =
        Button::ghost(SharedString::from(format!("{id}-unshare")), if unsharing { "Unsharing…" } else { "Unshare" })
            .small()
            .loading(unsharing)
            .disabled(unsharing)
            .on_click(on_unshare);
    let open = url.to_string();
    div()
        .flex()
        .items_center()
        .gap(px(space::MD))
        .py(px(space::SM + 2.))
        .px(px(space::MD))
        .rounded(px(radius::LG))
        .bg(colors.surface)
        .border_1()
        .border_color(colors.border)
        .child(ui::icon("globe").size(px(16.)).flex_none().text_color(colors.text_secondary))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().subhead().semibold().truncate().child(headline))
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-shared-link")))
                        .footnote()
                        .text_color(colors.text_secondary)
                        .truncate()
                        .cursor_pointer()
                        .hover(|style| style.text_color(colors.text))
                        .child(address)
                        .on_click(move |_, _, cx| cx.open_url(&open)),
                ),
        )
        .child(crate::smoke::tracked(format!("{id}-copy-link"), copy))
        .children(extra)
        .child(crate::smoke::tracked(format!("{id}-unshare"), unshare))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Share asks first, saying who sees it, what goes and stays, what goes unread and what was taken out.
    #[test]
    fn share_says_what_goes() {
        let preview = json!({
            "files": [{ "name": "after.png" }, { "name": "plan.pdf" }, { "name": "tests.log" }],
            "left": [{ "name": "core.bin", "why": "not a picture, recording, PDF or text" }],
            "asIs": ["after.png", "plan.pdf"],
            "hidden": 2,
        });
        assert_eq!(
            share_question(&preview),
            "Anyone with the link can open it, until you unshare it.\n\nGoes with the report: after.png, plan.pdf and tests.log.\n\n\
Stays on this Mac: core.bin (not a picture, recording, PDF or text).\n\n2 things that look like keys or tokens are taken out of the text. Check the report for others.\n\n\
after.png and plan.pdf go as they are: nothing reads them. They may show something private (a key on screen, a name, an address)."
        );
        let alone = share_question(&json!({ "files": [], "left": [], "asIs": [], "hidden": 0 }));
        assert!(alone.contains("Only the report goes.") && alone.contains("anything that looks like a key"), "{alone}");
    }

    #[test]
    fn a_picture_keeps_its_shape() {
        // Wide: the column's width.
        assert_eq!(picture_box(2200., 1740.), (640., 640. * 1740. / 2200.));
        // Tall: no taller than the cap, narrower instead of cropped.
        let (width, height) = picture_box(1120., 1740.);
        assert_eq!(height, PICTURE_HEIGHT);
        assert!((width / height - 1120. / 1740.).abs() < 0.001);
    }

    #[test]
    fn shows_the_rounds_own_media_in_place_and_nothing_from_elsewhere() {
        let files = vec![
            ("login works.png".to_string(), "image".to_string()),
            ("flow.mp4".to_string(), "video".to_string()),
            ("tests.log".to_string(), "file".to_string()),
        ];
        let urls = HashMap::from([("tests.log".to_string(), "https://worker.example/media/m/1".to_string())]);
        let got = segments(
            "Fixed it.\n\n![Login works](<login works.png>)\n\nSee [the tests](tests.log).\n\n- ![flow.mp4](<flow.mp4>) (upload pending)\n",
            &files,
            &urls,
        );
        assert_eq!(
            got,
            vec![
                Segment::Text("Fixed it.\n\n".into()),
                Segment::Media("login works.png".into()),
                Segment::Text("\n\nSee [the tests](https://worker.example/media/m/1).\n\n".into()),
                Segment::Media("flow.mp4".into()),
                Segment::Text(" (upload pending)\n".into()),
            ]
        );
        let outside = segments(
            "![tracker](https://evil.example/p.png) and ![missing](gone.png) and [secret](../../.ssh/id_rsa)",
            &files,
            &urls,
        );
        assert_eq!(
            outside,
            vec![Segment::Text("![tracker](https://evil.example/p.png) and missing and secret".into())]
        );
        // Any other file the round saved, embedded, is its row (a log the tool only named by path, listed after).
        assert_eq!(
            segments("Proof:\n\n![tests.log](<tests.log>)", &files, &urls),
            vec![Segment::Text("Proof:\n\n".into()), Segment::Media("tests.log".into())]
        );
        assert_eq!(strip_title("# Round 1: Fix\n\nBody"), "Body");
    }

    /// A report's page and its post's row are titled by its leading heading, of any level, which the page leaves out.
    #[test]
    fn a_report_is_titled_by_its_heading() {
        assert_eq!(
            leading_heading("## Release for friends (Nightly)\n- **Live now:** build 16"),
            Some(("Release for friends (Nightly)".into(), "- **Live now:** build 16".into()))
        );
        assert_eq!(leading_heading("\n# Status #\n\nAll good."), Some(("Status".into(), "All good.".into())));
        assert_eq!(report_title("Checked the build.\n\n## Details"), "Full report");
        assert_eq!(report_title("#hashtag first"), "Full report");
        assert_eq!(report_title("####### Too deep"), "Full report");
    }
}
