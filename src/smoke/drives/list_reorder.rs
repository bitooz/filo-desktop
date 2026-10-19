// `FILO_OPEN=list-reorder-timing`: the Agents list's rows moving to a new order, frame by frame.

use std::time::{Duration, Instant};

use gpui::{Context, Window};
use serde_json::json;

use super::{native_input, window_width};
use crate::app::AppShell;
use crate::nav::Tab;
use crate::smoke::DRIVE_BOUNDS;

/// Rows put in the list for the check, `row-0` the latest (`FILO_ROWS`, default 33).
fn rows() -> usize {
    std::env::var("FILO_ROWS").ok().and_then(|rows| rows.parse().ok()).unwrap_or(33)
}

/// Opens the Agents tab with `rows()` made-up rows in place of the agents, the pointer off the list, then moves rows to
/// the top as a new message does (`AgentsScreen::set_last_for_check`): one, one from below the list's edge, one while
/// another row's line changes every frame (a reply streaming in), three at once, and a second one while the first is
/// still moving. For each it logs every frame the window drew for half a second: when, how long since the frame before,
/// and where each moving row was (points from the list's top), then how far a row went in one frame at most, and
/// whether it started from where it showed (or the list's edge) and ended at its new place. With `FILO_DRIVE_SHOTS=<folder>`, pictures of the third and sixth frames (which
/// they slow). Nothing is saved; run it with a `FILO_CONFIG_DIR` of its own.
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
        wait(4000).await;
        this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx)).ok();
        wait(1500).await;
        let Some(screen) = this.update(cx, |shell, _| shell.agents_screen()).ok().flatten() else {
            eprintln!("list-reorder-timing: FAIL no Agents tab");
            return quit(cx);
        };
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
        native_input::hover(width, width * 0.75, 300.);
        wait(1500).await;
        let top = |id: &str| -> Option<(f32, Instant)> {
            let bounds = DRIVE_BOUNDS.lock().unwrap();
            let (bounds, at) = bounds.get(&format!("agent-row-{id}"))?;
            Some((f32::from(bounds.origin.y), *at))
        };
        let (Some((first, _)), Some((second, _))) = (top("row-0"), top("row-1")) else {
            eprintln!("list-reorder-timing: FAIL the rows aren't on screen");
            return quit(cx);
        };
        let row_height = second - first;
        // Where the list's first row is (the group's row may be above row-0).
        let start = screen.read_with(cx, |screen, _| screen.list_order()).unwrap_or_default();
        let list_top = first - row_height * start.iter().position(|key| key == "row-0").unwrap_or(0) as f32;
        // Where the list's part on screen ends, from its first row (the window's bottom, near enough).
        let height = cx.update(|window, _| f32::from(window.viewport_size().height)).unwrap_or(760.);
        let list_bottom = height - list_top;
        eprintln!("list-reorder-timing: rows {row_height} pt apart, {list_bottom} pt of the list on screen");
        let mut latest = crate::format::now_ms();
        let mut failures = 0;
        // (label, rows moved to the top at the start, a row moved `later` ms in, whether a row's line changes each frame)
        let runs: [(&str, &[usize], Option<(usize, u64)>, bool); 5] = [
            ("one row", &[9], None, false),
            ("one row from below the fold", &[25], None, false),
            ("one row while another streams", &[8], None, true),
            ("three rows at once", &[5, 7, 9], None, false),
            ("a second row while the first moves", &[9], Some((6, 70)), false),
        ];
        for (label, moved, later, streaming) in runs {
            let order = screen.read_with(cx, |screen, _| screen.list_order()).unwrap_or_default();
            let watched: Vec<String> = moved
                .iter()
                .chain(later.as_ref().map(|(index, _)| index))
                .map(|index| order.get(*index).cloned().unwrap_or_default())
                .collect();
            let before: Vec<Option<f32>> = watched.iter().map(|id| top(id).map(|(y, _)| y - list_top)).collect();
            // A row whose line changes each frame, as one does while its reply streams in.
            let streamer = order.first().cloned().unwrap_or_default();
            let started = Instant::now();
            for id in &watched[..moved.len()] {
                latest += 1000.;
                screen.update(cx, |screen, cx| screen.set_last_for_check(id, Some(latest), "New message", cx)).ok();
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
                let Some((_, at)) = top(&watched[0]) else { continue };
                if frames.last().is_some_and(|(last, _)| *last == at) {
                    continue;
                }
                frames.push((at, watched.iter().map(|id| top(id).map(|(y, _)| y - list_top)).collect()));
                // With `FILO_DRIVE_SHOTS`, pictures of the rows on their way (they slow those frames).
                if matches!(frames.len(), 3 | 6) {
                    let name = format!("list-reorder-{}-frame-{}", label.replace(' ', "-"), frames.len());
                    crate::window_shot::save_step(&name);
                }
            }
            eprintln!("list-reorder-timing: {label}: {watched:?} from {before:?}");
            let mut previous: Option<(Instant, Vec<Option<f32>>)> = None;
            let mut largest_step = vec![0f32; watched.len()];
            let mut gaps = Vec::new();
            for (at, tops) in &frames {
                let gap = previous.as_ref().map(|(last, _)| at.duration_since(*last));
                gaps.extend(gap);
                if let Some((_, last)) = &previous {
                    for (index, (now, then)) in tops.iter().zip(last).enumerate() {
                        if let (Some(now), Some(then)) = (now, then) {
                            largest_step[index] = largest_step[index].max((now - then).abs());
                        }
                    }
                }
                eprintln!(
                    "list-reorder-timing:   {:>5.1} ms (+{:>5.1}) {}",
                    at.saturating_duration_since(started).as_secs_f64() * 1000.,
                    gap.map_or(0., |gap| gap.as_secs_f64() * 1000.),
                    tops.iter().map(|y| y.map_or("-".into(), |y| format!("{y:>6.1}"))).collect::<Vec<_>>().join(" ")
                );
                previous = Some((*at, tops.clone()));
            }
            let order = screen.read_with(cx, |screen, _| screen.list_order()).unwrap_or_default();
            let ends: Vec<Option<f32>> = watched
                .iter()
                .map(|id| order.iter().position(|key| key == id).map(|index| index as f32 * row_height))
                .collect();
            let last = frames.last().map(|(_, tops)| tops.clone()).unwrap_or_default();
            let first = frames.first().map(|(_, tops)| tops.clone()).unwrap_or_default();
            let settled = last.iter().zip(&ends).all(|(at, end)| matches!((at, end), (Some(at), Some(end)) if (at - end).abs() < 0.5));
            // Each row starts from where it showed (or the list's edge, from below it), and goes there in steps of at
            // most a quarter of the way: the first frame drawn after the change, and every one after.
            let starts: Vec<Option<f32>> = before.iter().map(|was| was.map(|was| was.min(list_bottom))).collect();
            let distance = |index: usize| match (starts[index], ends[index]) {
                (Some(start), Some(end)) => (end - start).abs(),
                _ => 0.,
            };
            let from_where_it_was = first.iter().zip(&starts).enumerate().all(|(index, (at, start))| match (at, start) {
                (Some(at), Some(start)) => (at - start).abs() <= distance(index) / 4. + 1.,
                _ => false,
            });
            let smooth = largest_step.iter().enumerate().all(|(index, step)| *step <= distance(index) / 4. + 1.);
            gaps.sort();
            let max_gap = gaps.last().map_or(0., |gap| gap.as_secs_f64() * 1000.);
            let slow = gaps.iter().filter(|gap| gap.as_secs_f64() * 1000. > 20.).count();
            let ok = settled && from_where_it_was && smooth && max_gap < 34.;
            failures += usize::from(!ok);
            eprintln!(
                "list-reorder-timing: {label}: {} {} frames, longest gap {max_gap:.1} ms ({slow} over 20 ms), largest step {:?} pt, ends {:?} (wanted {:?}), started where it was: {from_where_it_was}, smooth: {smooth}",
                if ok { "OK" } else { "FAIL" },
                frames.len(),
                largest_step.iter().map(|step| step.round()).collect::<Vec<_>>(),
                last,
                ends,
            );
            wait(600).await;
        }
        eprintln!("list-reorder-timing: done, {failures} failed");
        quit(cx);
    })
    .detach();
}
