// What agents do in the browser, for the user to see and step in on (browser.rs): who works in each tab now, whether
// the user paused them there or took the tab over, and each agent's last steps (its trail), in words. The browser
// window shows them over the agent's page with Pause, Take over and Hand back (browser/window.rs), the agent's chat a
// badge that opens it (screens/agents.rs).

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// An agent counts as working in a tab this long after its last call there ended.
pub const WORKING_FOR: Duration = Duration::from_secs(6);
/// The steps kept per agent.
const TRAIL: usize = 30;

/// Who may act in a tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Control {
    /// The agent (the user can still watch).
    #[default]
    Agent,
    /// The user paused the agent there: its actions wait.
    Paused,
    /// The user took the tab over and works in it: the agent's actions wait until they hand it back.
    User,
}

/// The agent working in a tab, and who has it.
#[derive(Default)]
pub struct Work {
    /// The agent that acted there last.
    pub agent: Option<String>,
    /// Its calls going on there now.
    pub busy: u32,
    pub last: Option<Instant>,
    pub control: Control,
    /// What to tell the agent at its next call: the user paused it or took the tab over, and let it go on.
    pub told: Option<&'static str>,
}

impl Work {
    pub fn working(&self) -> bool {
        self.busy > 0 || self.last.is_some_and(|last| last.elapsed() < WORKING_FOR)
    }

    /// Whether the browser window shows the agent's bar for it: working, or held by the user.
    pub fn shown(&self) -> bool {
        self.agent.is_some() && (self.working() || self.control != Control::Agent)
    }
}

/// One step of an agent's, in words ("Clicked “Orders”").
#[derive(Clone, Debug)]
pub struct Step {
    pub tab: String,
    pub what: String,
    pub at: Instant,
}

thread_local! {
    static TRAILS: RefCell<HashMap<String, VecDeque<Step>>> = RefCell::new(HashMap::new());
}

/// Adds a step to `agent`'s trail; the same step again right after only moves its time.
pub fn record(agent: &str, tab: &str, what: String) {
    TRAILS.with(|trails| {
        let mut trails = trails.borrow_mut();
        let trail = trails.entry(agent.to_string()).or_default();
        if let Some(last) = trail.back_mut().filter(|last| last.tab == tab && last.what == what) {
            last.at = Instant::now();
            return;
        }
        trail.push_back(Step { tab: tab.to_string(), what, at: Instant::now() });
        while trail.len() > TRAIL {
            trail.pop_front();
        }
    });
}

/// `agent`'s last `count` steps, newest last.
pub fn trail(agent: &str, count: usize) -> Vec<Step> {
    TRAILS.with(|trails| {
        trails
            .borrow()
            .get(agent)
            .map(|trail| trail.iter().rev().take(count).rev().cloned().collect())
            .unwrap_or_default()
    })
}

/// How many steps all agents took: the browser window redraws when it changes.
pub fn steps() -> usize {
    TRAILS.with(|trails| trails.borrow().values().map(VecDeque::len).sum())
}

/// "4s ago", "2m ago".
pub fn ago(at: Instant) -> String {
    let seconds = at.elapsed().as_secs();
    match seconds {
        0..=4 => "now".into(),
        5..=59 => format!("{seconds}s ago"),
        60..=3599 => format!("{}m ago", seconds / 60),
        _ => format!("{}h ago", seconds / 3600),
    }
}

/// A step in words, for the op an agent called with `request`, from what it answered (`reply`, the tool's text, or
/// its error). Never what was typed: it may be a password.
pub fn step_of(op: &str, request: &serde_json::Value, reply: Result<&str, &str>) -> Option<String> {
    let text =
        |key: &str| request.get(key).and_then(serde_json::Value::as_str).map(str::trim).filter(|text| !text.is_empty());
    let answer = reply.unwrap_or_default();
    // The element it acted on: the name of the first element line in the answer ([e5] link "Orders"), else the first
    // name in quotes.
    let named = || {
        let quoted = |from: usize| {
            let start = from + answer[from..].find('"')? + 1;
            let end = start + answer[start..].find('"')?;
            let name = answer[start..end].trim();
            (!name.is_empty()).then(|| format!("“{}”", short(name, 48)))
        };
        let element = answer.find("[e").and_then(|at| {
            let rest = &answer[at..];
            let close = rest.find("] ")?;
            let after = &rest[close + 2..];
            let kind_end = after.find(' ')?;
            after[kind_end + 1..].starts_with('"').then(|| at + close + 2 + kind_end + 1)
        });
        element.and_then(quoted).or_else(|| quoted(0))
    };
    let host = |url: &str| {
        url::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(|host| host.trim_start_matches("www.").to_string()))
            .unwrap_or_else(|| short(url, 48))
    };
    let what = match op {
        "open" => format!("Opened {}", host(&crate::browser::address(text("url")?))),
        "navigate" => match (text("url"), text("go")) {
            (Some(url), _) => format!("Went to {}", host(&crate::browser::address(url))),
            (None, Some("back")) => "Went back".into(),
            (None, Some("forward")) => "Went forward".into(),
            _ => "Reloaded the page".into(),
        },
        "read" => match text("find") {
            Some(find) => format!("Looked for “{}”", short(find, 40)),
            None => "Read the page".into(),
        },
        "click" => format!("Clicked {}", named().unwrap_or_else(|| "on the page".into())),
        "click_at" => format!("Clicked {}", named().unwrap_or_else(|| "a spot".into())),
        "type" => format!("Typed into {}", named().unwrap_or_else(|| "a field".into())),
        "select" => {
            let mut options: Vec<String> = text("option").into_iter().map(String::from).collect();
            options.extend(
                request
                    .get("options")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(String::from),
            );
            let picked = options.iter().map(|option| format!("“{}”", short(option, 30))).collect::<Vec<_>>().join(", ");
            format!(
                "Picked {} in {}",
                if picked.is_empty() { "options".into() } else { picked },
                named().unwrap_or_else(|| "a menu".into())
            )
        }
        "upload" => format!("Chose {} for {}", files(request), named().unwrap_or_else(|| "a file field".into())),
        "hover" => format!("Pointed at {}", named().unwrap_or_else(|| "a spot".into())),
        "drag" => "Dragged on the page".into(),
        "keys" => match request.get("keys").and_then(serde_json::Value::as_array).filter(|keys| !keys.is_empty()) {
            Some(keys) => {
                format!("Pressed {}", keys.iter().filter_map(serde_json::Value::as_str).collect::<Vec<_>>().join(", "))
            }
            None => "Typed on the page".into(),
        },
        "scroll" => "Scrolled".into(),
        "screenshot" => "Took a screenshot".into(),
        "wait" => match text("text") {
            Some(text) => format!("Waited for “{}”", short(text, 40)),
            None => "Waited for the page".into(),
        },
        "dialog" => {
            format!("{} the page's dialog", if text("action") == Some("dismiss") { "Dismissed" } else { "Accepted" })
        }
        "close" => "Closed the tab".into(),
        "ask_user" => format!("Asked you: {}", short(text("reason")?, 60)),
        _ => return None,
    };
    Some(if reply.is_err() { format!("{what} (failed)") } else { what })
}

fn files(request: &serde_json::Value) -> String {
    let names: Vec<String> = request
        .get("paths")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(|path| {
            std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string())
        })
        .collect();
    match names.as_slice() {
        [] => "a file".into(),
        [one] => short(one, 40),
        [first, rest @ ..] => format!("{} and {} more", short(first, 40), rest.len()),
    }
}

fn short(text: &str, most: usize) -> String {
    if text.chars().count() > most {
        format!("{}…", text.chars().take(most - 1).collect::<String>())
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn steps_in_words_never_say_what_was_typed() {
        let typed = step_of(
            "type",
            &json!({ "text": "hunter2", "ref": "e3" }),
            Ok(r#"Typed into [e3] textbox "Password" value="•••" with real key presses."#),
        )
        .unwrap();
        assert_eq!(typed, "Typed into “Password”");
        assert!(!typed.contains("hunter2"));
        assert_eq!(
            step_of("click", &json!({}), Ok(r#"Clicked [e5] link "Orders" → https://a.b with real mouse events."#))
                .unwrap(),
            "Clicked “Orders”"
        );
        assert_eq!(
            step_of("open", &json!({ "url": "www.amazon.com/orders" }), Ok("Opened tab t1.")).unwrap(),
            "Opened amazon.com"
        );
        assert_eq!(step_of("click", &json!({}), Err("There's no element e9")).unwrap(), "Clicked on the page (failed)");
        assert_eq!(
            step_of("upload", &json!({ "paths": ["/tmp/a/cv.pdf"] }), Ok(r#"Chose cv.pdf for [e2] file "Resume""#))
                .unwrap(),
            "Chose cv.pdf for “Resume”"
        );
        assert_eq!(
            step_of(
                "select",
                &json!({ "option": "Cherry" }),
                Ok(r#"Picked "Cherry" in [e4] select "Fruit" selected="Cherry""#)
            )
            .unwrap(),
            "Picked “Cherry” in “Fruit”"
        );
        assert_eq!(
            step_of(
                "select",
                &json!({ "options": ["Red", "Blue"] }),
                Ok(r#"Picked "Red", "Blue" in [e5] select "Colors""#)
            )
            .unwrap(),
            "Picked “Red”, “Blue” in “Colors”"
        );
        assert_eq!(step_of("tabs", &json!({}), Ok("")), None);
    }

    #[test]
    fn a_step_again_moves_its_time_only() {
        record("Ada-test", "t1", "Scrolled".into());
        record("Ada-test", "t1", "Scrolled".into());
        record("Ada-test", "t1", "Read the page".into());
        let trail = trail("Ada-test", 10);
        assert_eq!(trail.iter().map(|step| step.what.as_str()).collect::<Vec<_>>(), ["Scrolled", "Read the page"]);
    }
}
