#!/usr/bin/env python3
"""Checks an artifact page before it's published.

    python3 check_page.py <page.html> [--widths 380,720,1100] [--shots <folder>]

It reads the file for what Filo's sandbox blocks or breaks (files loaded from the internet, browser storage), its
size, <title> and theme marker. Then, with Chrome on this Mac, it opens the page the way Filo shows it (in a sandboxed
frame of each width, Filo's theme at the marker, no network) and lists what sticks out past the page's edge or is cut
off by its box. --shots saves a screenshot at each width, to look at. It takes about half a minute.

Exit status: 0 when nothing needs fixing, 1 when something does, 2 when the page couldn't be read.
"""

import argparse
import html
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

MAX_BYTES = 2 * 1024 * 1024
MARKER = "<!--filo:head-->"
# Filo's Default light theme and type sizes, as the app puts them in at the marker.
THEME = (
    ":root{color-scheme:light;--background:#F6F5F2;--surface:#EBEAE7;--surfaceStrong:#E1E0DD;--raised:#FDFCF9;--sunken:#EFEEEB;"
    "--border:#CBCAC8;--text:#2A2927;--textSecondary:#4E4D4B;--textTertiary:#646361;--textDisabled:#80807D;--accent:#2B5BA8;"
    "--onAccent:#F7FAFF;--accentSoft:#CEDEF6;--onAccentSoft:#17396E;--success:#007332;--successSoft:#E1EEE3;--onSuccessSoft:#00481C;"
    "--danger:#B02A2D;--warning:#865900;--info:#006A95;--c1:#2a78d6;--c2:#eb6834;--c3:#1baf7a;--c4:#eda100;--title2:18px;--title3:16px;"
    "--headline:14px;--body:14px;--callout:14px;--subhead:13px;--footnote:12px;--caption:11px}"
    "html,body{font-family:system-ui,-apple-system,sans-serif}"
)
# Like the app's sandbox: nothing from the network, and filo.chat there to call.
HEAD = (
    "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; "
    "img-src data: blob:; font-src data:; media-src data: blob:\">"
    "<script>window.filo={chat(){}};</script>"
)
CHECK = r"""<script>(() => {
const run = () => {
  const width = document.documentElement.clientWidth;
  const out = { width, page: document.documentElement.scrollWidth, height: document.documentElement.scrollHeight, wide: [], cut: [] };
  const name = (el) => el.tagName.toLowerCase() + (el.id ? '#' + el.id : el.classList.length ? '.' + [...el.classList].slice(0, 2).join('.') : '');
  const path = (el) => { const parts = []; for (let e = el; e && e !== document.body && parts.length < 4; e = e.parentElement) parts.unshift(name(e)); return parts.join(' > '); };
  const words = (el) => (el.innerText || el.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 60);
  const scrolls = (el) => { for (let p = el.parentElement; p && p !== document.body; p = p.parentElement) { if (/(auto|scroll)/.test(getComputedStyle(p).overflowX)) return true; } return false; };
  const seen = new Set();
  const inSeen = (el) => { for (let p = el.parentElement; p; p = p.parentElement) if (seen.has(p)) return true; return false; };
  for (const el of document.body.querySelectorAll('*')) {
    if (el.id === 'filo-check' || ['SCRIPT', 'STYLE', 'TEMPLATE', 'NOSCRIPT'].includes(el.tagName)) continue;
    const style = getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden' || style.position === 'fixed') continue;
    const box = el.getBoundingClientRect();
    if (box.width === 0 && box.height === 0) continue;
    if ((box.right > width + 1 || box.left < -1) && !scrolls(el) && !inSeen(el)) {
      seen.add(el);
      out.wide.push({ el: path(el), right: Math.round(box.right), text: words(el) });
    }
    const hiddenX = /(hidden|clip)/.test(style.overflowX), hiddenY = /(hidden|clip)/.test(style.overflowY);
    const cutX = el.scrollWidth > el.clientWidth + 1, cutY = el.scrollHeight > el.clientHeight + 1;
    const ellipsis = style.textOverflow === 'ellipsis' && cutX;
    if (ellipsis || (hiddenX && cutX) || (hiddenY && cutY)) {
      out.cut.push({ el: path(el), how: ellipsis ? 'ends in an ellipsis' : 'overflow ' + (hiddenX && cutX ? style.overflowX : style.overflowY), text: words(el) });
    }
  }
  parent.postMessage({ filoCheck: out }, '*');
};
// At once, as the page is laid out, then again once it has loaded: the last one counts.
run();
addEventListener('load', () => (document.fonts ? document.fonts.ready.then(run) : run()));
})();</script>"""

REMOTE = re.compile(r"""(?:\bsrc\s*=\s*["']?|url\(\s*["']?|@import\s+["'])((?:https?:)?//[^"')\s>]+)""", re.I)
LINK = re.compile(r"""<link\b[^>]*\bhref\s*=\s*["']?((?:https?:)?//[^"'\s>]+)""", re.I)
STORAGE = re.compile(r"\b(localStorage|sessionStorage|indexedDB|document\.cookie)\b")


def find_chrome():
    found = os.environ.get("CHROME")
    if found and os.path.exists(found):
        return found
    home = os.path.expanduser("~")
    for app in ["Google Chrome", "Chromium", "Google Chrome Canary", "Microsoft Edge", "Brave Browser"]:
        for root in ["/Applications", os.path.join(home, "Applications")]:
            path = os.path.join(root, app + ".app", "Contents", "MacOS", app)
            if os.path.exists(path):
                return path
    for name in ["google-chrome", "chromium", "chromium-browser", "chrome", "microsoft-edge"]:
        if shutil.which(name):
            return shutil.which(name)
    return None


# The frame Filo shows a page in, as wide as asked: scripts but no origin (so no storage) and no network, the page put
# in as its srcdoc the way the app does. It keeps what the page's check finds, for --dump-dom.
def host_page(page, width, height):
    source = json.dumps(page).replace("<", "\\u003c")
    return (
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>html,body{margin:0;background:#8a8a8a}"
        f"iframe{{display:block;border:0;width:{width}px;height:{height}px}}</style></head>"
        "<body><iframe id=\"f\" sandbox=\"allow-scripts\"></iframe><pre id=\"filo-check\" hidden></pre><script>"
        "addEventListener('message', (e) => { if (e.data && e.data.filoCheck) document.getElementById('filo-check').textContent = JSON.stringify(e.data.filoCheck); });"
        f"document.getElementById('f').srcdoc = {source};</script></body></html>"
    )


def without_comments(page):
    return re.sub(r"<!--.*?-->", "", page, flags=re.S)


def checked_copy(page):
    """The page as the check opens it: Filo's theme at its marker, the sandbox's limits in its head, the check at the end."""
    if MARKER in page:
        page = page.replace(MARKER, "<style>" + THEME + "</style>" + MARKER, 1)
    head = re.search(r"<head\b[^>]*>", page, re.I)
    page = page[: head.end()] + HEAD + page[head.end():] if head else HEAD + page
    end = page.lower().rfind("</body>")
    return page[:end] + CHECK + page[end:] if end >= 0 else page + CHECK


def chrome(binary, profile, args, timeout=150):
    command = [binary, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--no-first-run", "--no-default-browser-check",
               "--user-data-dir=" + profile, "--virtual-time-budget=4000", "--disable-site-isolation-trials",
               "--disable-features=IsolateOrigins,site-per-process"] + args
    return subprocess.run(command, capture_output=True, text=True, timeout=timeout)


def main():
    parser = argparse.ArgumentParser(description="Check an artifact page before it's published.")
    parser.add_argument("page")
    parser.add_argument("--widths", default="380,720,1100", help="widths to check, in CSS pixels")
    parser.add_argument("--shots", help="a folder to save a screenshot at each width in")
    options = parser.parse_args()

    try:
        data = open(options.page, "rb").read()
        page = data.decode("utf-8")
    except (OSError, UnicodeDecodeError) as error:
        print(f"Couldn't read {options.page} as text: {error}")
        return 2
    problems = 0
    source = without_comments(page)
    title = re.search(r"<title[^>]*>(.*?)</title", source, re.I | re.S)
    print(f"{options.page}: {len(data) // 1024} KB, title " + (f'"{html.unescape(title.group(1)).strip()}"' if title and title.group(1).strip() else "missing (add a <title>)"))
    if len(data) > MAX_BYTES:
        problems += 1
        print(f"FIX: it's bigger than Filo takes ({MAX_BYTES // 1024} KB at most). Make the pictures smaller or fewer.")
    if MARKER not in page:
        print(f"NOTE: no {MARKER} in its <head>, so it keeps its own look rather than the app's theme and font.")
    remote = sorted(set(REMOTE.findall(source) + LINK.findall(source)))
    if remote:
        problems += 1
        print("FIX: it loads from the internet, which Filo's sandbox blocks (put it in the page: inline <style> and <script>, data: URLs, inline SVG):")
        for url in remote[:8]:
            print(f"  - {url}")
    storage = sorted(set(STORAGE.findall(source)))
    if storage:
        problems += 1
        print(f"FIX: it uses {', '.join(storage)}, which throws in Filo's sandbox (no storage there): keep its state in memory.")

    binary = find_chrome()
    if not binary:
        print("Chrome isn't on this Mac, so the widths weren't checked: read the page for fixed widths, nowrap and overflow:hidden instead.")
        return 1 if problems else 0
    widths = [int(width) for width in options.widths.split(",") if width.strip().isdigit()]
    with tempfile.TemporaryDirectory(prefix="filo-check-") as temp:
        copy = checked_copy(page)
        host = os.path.join(temp, "host.html")
        url = "file://" + host
        for width in widths:
            with open(host, "w", encoding="utf-8") as file:
                file.write(host_page(copy, width, 900))
            # Chrome's window is at least 500 wide; the frame is the width that counts.
            window = max(width, 500)
            try:
                ran = chrome(binary, os.path.join(temp, "profile"), [f"--window-size={window},900", "--dump-dom", url])
            except OSError as error:
                print(f"Couldn't run Chrome ({error}): read the page for fixed widths, nowrap and overflow:hidden instead.")
                return 1 if problems else 0
            except subprocess.TimeoutExpired:
                print(f"At {width}px: Chrome took too long (this Mac may be busy); check again, or read the page for fixed widths, nowrap and overflow:hidden.")
                continue
            found = re.search(r'<pre id="filo-check" hidden="">(.+?)</pre>', ran.stdout, re.S)
            if not found:
                print(f"At {width}px: the page didn't finish loading in Chrome (a script error?). {ran.stderr.strip()[-300:]}")
                problems += 1
                continue
            report = json.loads(html.unescape(found.group(1)))
            lines = []
            if report["page"] > report["width"] + 1:
                lines.append(f"the page is {report['page']}px wide, wider than the window: it scrolls sideways")
            for item in report["wide"][:6]:
                lines.append(f"sticks out to {item['right']}px: {item['el']}" + (f" (“{item['text']}”)" if item["text"] else ""))
            for item in report["cut"][:6]:
                lines.append(f"cut off ({item['how']}): {item['el']}" + (f" (“{item['text']}”)" if item["text"] else ""))
            if lines:
                problems += 1
                print(f"FIX at {width}px:")
                for line in lines:
                    print(f"  - {line}")
            else:
                print(f"At {width}px: nothing sticks out or is cut off.")
            if options.shots:
                os.makedirs(options.shots, exist_ok=True)
                shot = os.path.abspath(os.path.join(options.shots, f"{os.path.splitext(os.path.basename(options.page))[0]}-{width}.png"))
                height = max(400, min(int(report.get("height", 900)), 5000))
                with open(host, "w", encoding="utf-8") as file:
                    file.write(host_page(copy, width, height))
                try:
                    chrome(binary, os.path.join(temp, "profile"), [f"--window-size={window},{height}", f"--screenshot={shot}", url])
                    print(f"  screenshot: {shot}")
                except (OSError, subprocess.TimeoutExpired) as error:
                    print(f"  no screenshot: {error}")
    if problems:
        print("Fix those, then check again before you publish.")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
