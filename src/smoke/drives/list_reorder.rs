// `FILO_OPEN=list-reorder-timing`: the Agents list's rows moving to a new order, frame by frame.

use std::time::{Duration, Instant};

use gpui::{Context, Window};
use serde_json::json;

use super::{native_input, window_width};
use crate::app::AppShell;
use crate::nav::Tab;
use crate::sliding_rows::check;
use crate::smoke::DRIVE_BOUNDS;

/// Rows put in the list for the check, `row-0` the latest (`FILO_ROWS`, default 33).
fn rows() -> usize {
    std::env::var("FILO_ROWS").ok().and_then(|rows| rows.parse().ok()).unwrap_or(33)
}

/// The rows' column (`AgentsScreen::render_list`).
const COLUMN: &str = "agents-rows";

/// Where row `id` was drawn last (window points) and when.
fn row_top(id: &str) -> Option<(f32, Instant)> {
    let bounds = DRIVE_BOUNDS.lock().unwrap();
    let (bounds, at) = bounds.get(&format!("agent-row-{id}"))?;
    Some((f32::from(bounds.origin.y), *at))
}

fn row_bounds(id: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    DRIVE_BOUNDS.lock().unwrap().get(&format!("agent-row-{id}")).map(|(bounds, _)| *bounds)
}

/// The part of the list in view, in points from the rows' column's top: (column top in the window, view top, view
/// bottom).
fn view() -> Option<(f32, f32, f32)> {
    check::shown_part(COLUMN).map(|(column, top, bottom)| (column, top - column, bottom - column))
}

/// Scrolls the list `dy` points (down is positive). A posted wheel lands off where it's aimed: one that scrolls nothing,
/// aimed near the window's top left, tells by how much (gpui takes the pointer to where a wheel lands), then one over
/// the list (as `run_agent_model_drive` does).
async fn scroll_list(width: f64, dy: f64, cx: &mut gpui::AsyncWindowContext) {
    let executor = cx.background_executor().clone();
    native_input::scroll_in(width, 40., 200., 0.);
    executor.timer(Duration::from_millis(300)).await;
    let landed = cx.update(|window, _| window.mouse_position()).unwrap_or_default();
    let (off_x, off_y) = (f64::from(f32::from(landed.x)) - 40., f64::from(f32::from(landed.y)) - 200.);
    native_input::scroll_in(width, 230. - off_x, 400. - off_y, dy);
    executor.timer(Duration::from_millis(600)).await;
    eprintln!("list-reorder-timing: scrolled {dy} pt: view {:?}", view().map(|(_, top, bottom)| (top, bottom)));
}

/// What a run does to the rows.
enum Move {
    /// Rows (by place in the list) get a new message, moving to the top; with a row moved `later` ms in, and with the
    /// top row's line changing every frame (a reply streaming in).
    Up { rows: &'static [usize], later: Option<(usize, u64)>, streaming: bool },
    /// The row at this place gets an old last message, moving far down (a draft cleared, a question answered).
    Down(usize),
}

/// Opens the Agents tab with `rows()` made-up rows in place of the agents, the pointer off the list, then moves rows as
/// new messages do (`AgentsScreen::set_last_for_check`): one up, one up from below the list's edge, one up while
/// another row's line changes every frame (a reply streaming in), three at once, a second while the first is still on
/// its way, one far down, and one up with the list scrolled down (its place out of view). For each it logs every frame
/// drawn for half a second: when, how long since the frame before, and where each moving row was (points from the
/// list's top), and checks each row started where it showed and went to its place, or to the edge of the view when that
/// was out of it, never more than a quarter of the way in a frame (the first one drawn included), and ended in its
/// place, with no frame 34 ms or more after the one before. Then, with slides slowed, it clicks where a row moving up
/// covers another and checks the row on top opened. With `FILO_DRIVE_SHOTS=<folder>`, pictures of the third and sixth
/// frames (which they slow). Nothing is saved; run it with a `FILO_CONFIG_DIR` of its own.
pub(super) fn run(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let quit = |cx: &mut gpui::AsyncWindowContext| {
            if std::env::var_os("FILO_DRIVE_STAY").is_none() {
                cx.update(|_, cx| cx.quit()).ok();
            }
        };
        let mut screen = None;
        for _ in 0..100 {
            screen = this.update(cx, |shell, _| shell.agents_screen()).ok().flatten();
            if screen.is_some() {
                break;
            }
            wait(100).await;
        }
        let Some(screen) = screen else {
            eprintln!("list-reorder-timing: FAIL no Agents tab");
            return quit(cx);
        };
        // The tab's chat and the list's own queries come in.
        wait(3000).await;
        let now = crate::format::now_ms();
        let rows: Vec<_> = (0..rows())
            .map(|index| {
                json!({
                    "_id": format!("row-{index}"),
                    "name": format!("Agent {index}"),
                    "picture": if index % 2 == 0 { "panda-waving" } else { "bear-waving" },
                    "instructions": "",
                    "last": { "fromUser": false, "preview": format!("Message {index}"), "at": now - 60_000. * (index as f64 + 1.) },
                    "unseen": 0,
                })
            })
            .filter_map(|row| serde_json::from_value(row).ok())
            .collect();
        screen.update(cx, |screen, cx| screen.show_rows_for_check(rows, cx)).ok();
        let Some(width) = window_width(&this, cx) else { return quit(cx) };
        // The list holds its order while the pointer is over it.
        let away = || native_input::hover(width, width * 0.75, 300.);
        away();
        wait(1500).await;
        let (Some((first, _)), Some((second, _)), Some(_)) = (row_top("row-0"), row_top("row-1"), view()) else {
            eprintln!("list-reorder-timing: FAIL the rows aren't on screen");
            return quit(cx);
        };
        let row_height = second - first;
        let order = |cx: &mut gpui::AsyncWindowContext| screen.read_with(cx, |screen, _| screen.list_order());
        let mut latest = crate::format::now_ms();
        let mut failures = 0;
        let runs: [(&str, Move, bool); 7] = [
            ("one row up", Move::Up { rows: &[9], later: None, streaming: false }, false),
            ("one row up from below the view", Move::Up { rows: &[25], later: None, streaming: false }, false),
            ("one row up while another streams", Move::Up { rows: &[8], later: None, streaming: true }, false),
            ("three rows up at once", Move::Up { rows: &[5, 7, 9], later: None, streaming: false }, false),
            ("a second row up while the first moves", Move::Up { rows: &[9], later: Some((6, 70)), streaming: false }, false),
            ("one row far down", Move::Down(1), false),
            ("one row up, the list scrolled down", Move::Up { rows: &[16], later: None, streaming: false }, true),
        ];
        for (label, change, scrolled) in runs {
            if scrolled {
                // Down the list, then the pointer off it again.
                scroll_list(width, 500., cx).await;
                away();
                wait(600).await;
            }
            let Some((column, view_top, view_bottom)) = view() else { break };
            let at = |id: &str| row_top(id).map(|(y, _)| y - column);
            let within = |y: f32| y.clamp(view_top - row_height, view_bottom);
            let shown = order(cx).unwrap_or_default();
            let (moved, later): (Vec<usize>, _) = match &change {
                Move::Up { rows, later, .. } => (rows.to_vec(), *later),
                Move::Down(place) => (vec![*place], None),
            };
            let watched: Vec<String> = moved
                .iter()
                .chain(later.as_ref().map(|(place, _)| place))
                .map(|place| shown.get(*place).cloned().unwrap_or_default())
                .collect();
            let before: Vec<Option<f32>> = watched.iter().map(|id| at(id)).collect();
            let streamer = shown.first().cloned().unwrap_or_default();
            let streaming = matches!(change, Move::Up { streaming: true, .. });
            let started = Instant::now();
            for id in &watched[..moved.len()] {
                latest += 1000.;
                let when = if matches!(change, Move::Down(_)) { now - 1e9 } else { latest };
                screen.update(cx, |screen, cx| screen.set_last_for_check(id, Some(when), "New message", cx)).ok();
            }
            let mut frames: Vec<(Instant, Vec<Option<f32>>)> = Vec::new();
            let mut later_done = later.is_none();
            let mut token = 0;
            while started.elapsed() < Duration::from_millis(500) {
                if let Some((_, after)) = later
                    && !later_done
                    && started.elapsed() >= Duration::from_millis(after)
                {
                    latest += 1000.;
                    let id = watched.last().cloned().unwrap_or_default();
                    screen.update(cx, |screen, cx| screen.set_last_for_check(&id, Some(latest), "New message", cx)).ok();
                    later_done = true;
                }
                if streaming {
                    token += 1;
                    let text = format!("Message 0{}", " word".repeat(token % 12));
                    screen.update(cx, |screen, cx| screen.set_last_for_check(&streamer, None, &text, cx)).ok();
                }
                let (tx, rx) = futures::channel::oneshot::channel();
                cx.update(|window, _| {
                    window.on_next_frame(move |_, _| {
                        let _ = tx.send(());
                    })
                })
                .ok();
                if rx.await.is_err() {
                    break;
                }
                let Some((_, drawn)) = row_top(&watched[0]) else { continue };
                if frames.last().is_some_and(|(last, _)| *last == drawn) {
                    continue;
                }
                frames.push((drawn, watched.iter().map(|id| at(id)).collect()));
                // With `FILO_DRIVE_SHOTS`, pictures of the rows on their way (they slow those frames).
                if matches!(frames.len(), 3 | 6) {
                    let name = format!("list-reorder-{}-frame-{}", label.replace([' ', ','], "-"), frames.len());
                    crate::window_shot::save_step(&name);
                }
            }
            eprintln!("list-reorder-timing: {label}: {watched:?} from {before:?}, view {view_top}..{view_bottom}");
            let shown = order(cx).unwrap_or_default();
            // Where each row ends: its place (rows are `row_height` apart).
            let ends: Vec<Option<f32>> = watched
                .iter()
                .map(|id| shown.iter().position(|key| key == id).map(|place| place as f32 * row_height))
                .collect();
            // How far each goes in view: from where it showed to its place, each kept to the edge of the view.
            let distance: Vec<f32> = before
                .iter()
                .zip(&ends)
                .map(|(was, end)| match (was, end) {
                    (Some(was), Some(end)) => (within(*end) - within(*was)).abs(),
                    _ => 0.,
                })
                .collect();
            let mut previous: Option<(Instant, Vec<Option<f32>>)> = None;
            let mut largest_step = vec![0f32; watched.len()];
            let mut gaps = Vec::new();
            for (drawn, tops) in &frames {
                let gap = previous.as_ref().map(|(last, _)| drawn.duration_since(*last));
                gaps.extend(gap);
                // The first frame's step is from where the row showed.
                let last = previous.as_ref().map_or(&before, |(_, tops)| tops);
                for (index, (now, then)) in tops.iter().zip(last).enumerate() {
                    if let (Some(now), Some(then)) = (now, then) {
                        largest_step[index] = largest_step[index].max((within(*now) - within(*then)).abs());
                    }
                }
                eprintln!(
                    "list-reorder-timing:   {:>5.1} ms (+{:>5.1}) {}",
                    drawn.saturating_duration_since(started).as_secs_f64() * 1000.,
                    gap.map_or(0., |gap| gap.as_secs_f64() * 1000.),
                    tops.iter().map(|y| y.map_or("-".into(), |y| format!("{y:>7.1}"))).collect::<Vec<_>>().join(" ")
                );
                previous = Some((*drawn, tops.clone()));
            }
            let last = frames.last().map(|(_, tops)| tops.clone()).unwrap_or_default();
            let settled = last.len() == ends.len()
                && last.iter().zip(&ends).all(|(at, end)| matches!((at, end), (Some(at), Some(end)) if (at - end).abs() < 0.5));
            let smooth = !frames.is_empty()
                && largest_step.iter().zip(&distance).all(|(step, distance)| *step <= distance / 4. + 1.);
            gaps.sort();
            let max_gap = gaps.last().map_or(0., |gap| gap.as_secs_f64() * 1000.);
            let ok = settled && smooth && max_gap < 34.;
            failures += usize::from(!ok);
            eprintln!(
                "list-reorder-timing: {label}: {} {} frames, longest gap {max_gap:.1} ms, largest step {:?} pt of {:?} in view, ends {:?} (wanted {:?}), smooth: {smooth}",
                if ok { "OK" } else { "FAIL" },
                frames.len(),
                largest_step.iter().map(|step| step.round()).collect::<Vec<_>>(),
                distance.iter().map(|distance| distance.round()).collect::<Vec<_>>(),
                last,
                ends,
            );
            if scrolled {
                scroll_list(width, -3000., cx).await;
                away();
            }
            wait(600).await;
        }
        // A click where a row moving up covers another opens the row on top. Slowed, so it's still there when the
        // click comes.
        check::slow_down(Some(Duration::from_millis(2000)));
        let shown = order(cx).unwrap_or_default();
        let riser = shown.get(10).cloned().unwrap_or_default();
        latest += 1000.;
        screen.update(cx, |screen, cx| screen.set_last_for_check(&riser, Some(latest), "New message", cx)).ok();
        wait(700).await;
        let covered = row_bounds(&riser).and_then(|top| {
            shown
                .iter()
                .filter(|id| **id != riser)
                .filter_map(|id| Some((id.clone(), row_bounds(id)?.intersect(&top))))
                .filter(|(_, overlap)| f32::from(overlap.size.height) >= 8.)
                .max_by(|a, b| f32::from(a.1.size.height).total_cmp(&f32::from(b.1.size.height)))
        });
        let clicked = match covered {
            Some((under, overlap)) => {
                let center = overlap.center();
                native_input::click(width, f64::from(f32::from(center.x)), f64::from(f32::from(center.y)));
                wait(300).await;
                let opened = screen.read_with(cx, |screen, _| screen.drive_selected()).ok().flatten();
                let ok = opened.as_deref() == Some(riser.as_str());
                eprintln!(
                    "list-reorder-timing: click where {riser} (moving up) covers {under} by {:.0} pt: opened {opened:?}: {}",
                    f32::from(overlap.size.height),
                    if ok { "OK" } else { "FAIL" }
                );
                ok
            }
            None => {
                eprintln!("list-reorder-timing: click: FAIL no row under {riser} on its way");
                false
            }
        };
        failures += usize::from(!clicked);
        check::slow_down(None);
        away();
        wait(2000).await;
        eprintln!("list-reorder-timing: done, {failures} failed");
        quit(cx);
    })
    .detach();
}
