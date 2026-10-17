// A glance as one self-contained page: the theme roles, the chart helpers and the
// tooltip runtime, then the fragment Filo wrote. The app shows it in a sandboxed frame and adds its own theme and font
// where `<!--filo:head-->` sits (src/screens/chat/glance.rs); saved to Artifacts it stands alone, light or dark with
// the system. Ported from the old hosted backend's convex/lib/glances/document.ts.

/// The fragment Filo wrote, at most this long.
pub const MAX_HTML: usize = 60_000;
/// The glance in words, at most this long.
pub const MAX_TEXT: usize = 4_000;

/// The Default family's roles a glance may use (assets/themes.json), light and dark.
const LIGHT: &[(&str, &str)] = &[
    ("background", "#F6F5F2"),
    ("surface", "#EBEAE7"),
    ("surfaceStrong", "#E1E0DD"),
    ("raised", "#FDFCF9"),
    ("sunken", "#EFEEEB"),
    ("border", "#CBCAC8"),
    ("text", "#2A2927"),
    ("textSecondary", "#4E4D4B"),
    ("textTertiary", "#646361"),
    ("textDisabled", "#80807D"),
    ("accent", "#2B5BA8"),
    ("onAccent", "#F7FAFF"),
    ("accentSoft", "#CEDEF6"),
    ("onAccentSoft", "#17396E"),
    ("success", "#007332"),
    ("danger", "#B02A2D"),
    ("warning", "#865900"),
    ("info", "#006A95"),
    ("c1", "#2a78d6"),
    ("c2", "#eb6834"),
    ("c3", "#1baf7a"),
    ("c4", "#eda100"),
];
const DARK: &[(&str, &str)] = &[
    ("background", "#121214"),
    ("surface", "#1D1D1F"),
    ("surfaceStrong", "#29292C"),
    ("raised", "#38383B"),
    ("sunken", "#09090B"),
    ("border", "#434446"),
    ("text", "#DEDEE0"),
    ("textSecondary", "#B1B2B4"),
    ("textTertiary", "#959698"),
    ("textDisabled", "#6B6C6E"),
    ("accent", "#8DB8F5"),
    ("onAccent", "#060B14"),
    ("accentSoft", "#263244"),
    ("onAccentSoft", "#AFD0FF"),
    ("success", "#5DC879"),
    ("danger", "#FF8D85"),
    ("warning", "#E6A024"),
    ("info", "#28BBFE"),
    ("c1", "#3987e5"),
    ("c2", "#d95926"),
    ("c3", "#199e70"),
    ("c4", "#c98500"),
];

/// The theme roles as CSS variables, light or dark with the system.
pub fn theme_css() -> String {
    let vars = |palette: &[(&str, &str)]| {
        palette.iter().map(|(role, value)| format!("--{role}:{value}")).collect::<Vec<_>>().join(";")
    };
    format!(
        ":root{{{};color-scheme:light}}@media (prefers-color-scheme:dark){{:root{{{};color-scheme:dark}}}}",
        vars(LIGHT),
        vars(DARK)
    )
}

/// What Filo's fragments can rely on: sizes, the legend, axis, gridline and table helpers, and the tooltip's look.
const BASE_CSS: &str = r#"
:root{--title2:19.5px;--title3:17px;--headline:15px;--body:15px;--callout:15px;--subhead:14px;--footnote:13px;--caption:12px}
*{box-sizing:border-box}
html,body{margin:0;background:transparent;color:var(--text);font:var(--body)/1.45 Atkinson,system-ui,-apple-system,sans-serif;-webkit-font-smoothing:antialiased}
body{overflow:hidden;position:relative}
html[data-size=large] body,html[data-standalone] body{overflow:auto}
html[data-standalone]{background:var(--background)}
html[data-standalone] body{background:var(--background);padding:24px;max-width:860px;margin:0 auto}
.num{font-variant-numeric:tabular-nums}
.muted{color:var(--textTertiary)}
.legend-row{display:flex;flex-wrap:wrap;align-items:center;gap:4px 14px;margin-top:10px;color:var(--textSecondary);font-size:var(--caption)}
.legend-row>span{display:inline-flex;align-items:center;gap:6px}
.legend-row .grow{flex:1}
.legend-row .muted{color:var(--textTertiary)}
.key-line{display:inline-block;width:12px;height:2px;border-radius:1px;background:var(--k);flex:none}
.key-box{display:inline-block;width:10px;height:10px;border-radius:3px;background:var(--k);flex:none}
.axis{color:var(--textTertiary);font-size:11px;font-variant-numeric:tabular-nums;white-space:nowrap}
.grid-h{position:absolute;left:0;right:0;height:1px;background:color-mix(in srgb,var(--border) 50%,transparent)}
.grid-h.base{background:var(--border)}
.hit{outline:none}
.hit:focus-visible{outline:2px solid var(--text);outline-offset:1px}
.dt{width:100%;border-collapse:collapse;font-size:var(--subhead)}
.dt th{text-align:left;font-weight:500;color:var(--textTertiary);font-size:var(--caption);padding:0 10px 6px;white-space:nowrap}
.dt td{padding:7px 10px;border-top:1px solid color-mix(in srgb,var(--border) 45%,transparent);font-variant-numeric:tabular-nums;white-space:nowrap}
.dt .r{text-align:right}
.dt .key-box,.dt .key-line{margin-right:8px;vertical-align:1px}
.ctip{position:absolute;z-index:25;pointer-events:none;min-width:130px;padding:7px 10px 8px;border-radius:10px;background:var(--raised);border:1px solid var(--border);box-shadow:0 8px 24px rgba(0,0,0,.3);font-size:var(--caption);color:var(--textSecondary)}
.ctip h4{margin:0 0 4px;font-size:var(--caption);font-weight:600;color:var(--text)}
.ctip .tr{display:flex;align-items:center;gap:7px;line-height:1.6}
.ctip .tr b{color:var(--text);font-weight:600;font-variant-numeric:tabular-nums}
.filo-notes{margin-top:18px;padding:12px 14px;border-radius:14px;background:var(--sunken);font-size:var(--subhead);color:var(--textSecondary)}
.filo-notes p{margin:0}
.filo-notes p+p{margin-top:6px}
html:not([data-size=large]):not([data-standalone]) .filo-notes{display:none}
.filo-title{margin:0 0 4px;font-size:var(--title3);font-weight:600}
.filo-asof{margin:0 0 16px;color:var(--textTertiary);font-size:var(--footnote)}
"#;

/// Chart tooltips: a mark's `data-tt` holds `{t, rows: [[colour, label, value, 'line'|'box']]}` as JSON (or URI-encoded
/// JSON). The tip follows the pointer over a mark, or sits by a mark with keyboard focus.
const TOOLTIP_JS: &str = r#"(()=>{let tip=null;
const hide=()=>{if(tip){tip.remove();tip=null}};
function show(el,px,py){const b=document.body,c=b.getBoundingClientRect();
if(!tip||tip.dataset.src!==el.dataset.tt){hide();let d;const raw=el.dataset.tt;try{d=JSON.parse(raw)}catch{try{d=JSON.parse(decodeURIComponent(raw))}catch{return}}
tip=document.createElement('div');tip.className='ctip';tip.dataset.src=el.dataset.tt;tip.setAttribute('role','tooltip');
const h=document.createElement('h4');h.textContent=String(d.t??'');tip.appendChild(h);
for(const [k,label,value,shape] of (Array.isArray(d.rows)?d.rows:[])){const r=document.createElement('div');r.className='tr';
const key=document.createElement('i');key.className=shape==='line'?'key-line':'key-box';if(k)key.style.setProperty('--k',k);else key.style.visibility='hidden';
const v=document.createElement('b');v.textContent=String(value??'');const s=document.createElement('span');s.textContent=String(label??'');
r.append(key,v,s);tip.appendChild(r)}b.appendChild(tip)}
const w=tip.offsetWidth,h=tip.offsetHeight,W=Math.max(b.scrollWidth,innerWidth),H=Math.max(b.scrollHeight,innerHeight);
let x=px-c.left+14,y=py-c.top+16;if(x+w>W-8)x=px-c.left-w-14;if(y+h>H-4)y=py-c.top-h-12;
tip.style.left=Math.max(4,x)+'px';tip.style.top=Math.max(4,y)+'px'}
document.addEventListener('pointermove',e=>{const el=e.target.closest&&e.target.closest('[data-tt]');if(el)show(el,e.clientX,e.clientY);else hide()});
document.documentElement.addEventListener('pointerleave',hide);
document.addEventListener('focusin',e=>{const el=e.target.closest&&e.target.closest('[data-tt]');if(!el)return hide();const r=el.getBoundingClientRect();show(el,r.left+r.width/2,r.top+r.height/2)});
document.addEventListener('focusout',e=>{if(e.target.closest&&e.target.closest('[data-tt]'))hide()});
document.addEventListener('scroll',hide,true)})();"#;

const CSP: &str =
    "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; font-src data:";

pub fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// What a glance's page is made of.
#[derive(Clone, Copy, Debug, Default)]
pub struct Input<'a> {
    pub title: &'a str,
    pub as_of: Option<&'a str>,
    pub html: &'a str,
    pub notes: Option<&'a str>,
    /// A page of its own (Artifacts): title on top, page background, light or dark with the system.
    pub standalone: bool,
}

/// The glance inside `<body>`: its title on a page of its own, the fragment, then the notes (shown opened larger).
fn body(glance: &Input) -> String {
    let notes = match glance.notes.map(str::trim).filter(|notes| !notes.is_empty()) {
        Some(notes) => {
            let lines: String = notes
                .split('\n')
                .filter(|line| !line.is_empty())
                .map(|line| format!("<p>{}</p>", escape_html(line)))
                .collect();
            format!("<div class=\"filo-notes\">{lines}</div>")
        }
        None => String::new(),
    };
    let head = if glance.standalone {
        let as_of = glance
            .as_of
            .filter(|as_of| !as_of.is_empty())
            .map(|as_of| format!("<p class=\"filo-asof\">{}</p>", escape_html(as_of)))
            .unwrap_or_default();
        format!("<h1 class=\"filo-title\">{}</h1>{as_of}", escape_html(glance.title))
    } else {
        String::new()
    };
    format!("{head}<div id=\"filo-glance\">{}</div>{notes}", glance.html)
}

/// The whole page: what `glances:get` gives the app, and (standalone) what Save to Artifacts keeps.
pub fn document(glance: &Input) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"{standalone}>\n<head>\n<meta charset=\"utf-8\">\n<meta http-equiv=\"Content-Security-Policy\" content=\"{CSP}\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{title}</title>\n<style>{theme}{BASE_CSS}</style>\n<!--filo:head-->\n</head>\n<body>{body}<script>{TOOLTIP_JS}</script></body>\n</html>",
        standalone = if glance.standalone { " data-standalone" } else { "" },
        title = escape_html(glance.title),
        theme = theme_css(),
        body = body(glance),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_has_the_theme_the_fragment_and_the_marker() {
        let input = Input {
            title: "Week <1>",
            as_of: Some("Oct 4"),
            html: "<div class=\"chart\">x</div>",
            notes: Some("First.\n\nSecond & last."),
            standalone: false,
        };
        let page = document(&input);
        assert!(page.starts_with("<!doctype html>\n<html lang=\"en\">\n"));
        assert!(page.contains("<title>Week &lt;1&gt;</title>"));
        assert!(page.contains("<!--filo:head-->\n</head>"));
        assert!(
            page.contains("--surfaceStrong:#E1E0DD")
                && page.contains("@media (prefers-color-scheme:dark){:root{--background:#121214")
        );
        assert!(page.contains("<body><div id=\"filo-glance\"><div class=\"chart\">x</div></div><div class=\"filo-notes\"><p>First.</p><p>Second &amp; last.</p></div><script>"));
        // Inline, the card's head has the title: the page doesn't.
        assert!(!page.contains("filo-title\">"));

        let saved = document(&Input { standalone: true, notes: None, ..input });
        assert!(saved.contains("<html lang=\"en\" data-standalone>"));
        assert!(saved.contains(
            "<body><h1 class=\"filo-title\">Week &lt;1&gt;</h1><p class=\"filo-asof\">Oct 4</p><div id=\"filo-glance\">"
        ));
        assert!(!saved.contains("<div class=\"filo-notes\">"));
    }

    #[test]
    fn escaping_covers_quotes() {
        assert_eq!(escape_html(r#"<a href="x">'&'</a>"#), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
    }
}
