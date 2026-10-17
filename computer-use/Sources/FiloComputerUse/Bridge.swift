// Filo's computer use bridge: one JSON request per line on stdin, one JSON reply per line on stdout. A chat turn's
// `use_computer` server (crates/filo-local/src/computer.rs) starts it on its first call and stops it when it's idle,
// when the turn ends or when the user stops Filo. Arguments arrive as JSON values, never through a shell. While it acts, the user
// sees where: a cursor of Filo's own glides to what it clicks or scrolls, and the app's window gets a coloured border
// (the library's visual effects, which need AppKit running on the main thread, so requests are read on another).

import AppKit
import CoreGraphics
import Foundation
import KWWKComputerUseCore

struct Request: Decodable {
    var id: String
    var command: String
    var args: Args?
}

struct Args: Decodable {
    var app: String?
    var window: String?
    var element: Int?
    var text: String?
    var value: String?
    var key: String?
    var direction: String?
    var pages: Double?
    var action: String?
    /// A point in the last screenshot, in its pixels (click, drag from), and where a drag ends.
    var x: Double?
    var y: Double?
    var toX: Double?
    var toY: Double?
    /// look, and every action: also take a screenshot of the window (after acting, for an action).
    var screenshot: Bool?
}

struct Reply: Encodable {
    var id: String
    var ok: Bool
    var text: String
    var error: String?
}

/// Longest text the bridge types or sets in one go.
let maxText = 10_000

struct BridgeError: Error, CustomStringConvertible {
    var description: String
    init(_ description: String) { self.description = description }
}

func need<T>(_ value: T?, _ name: String) throws -> T {
    guard let value else { throw BridgeError("Missing \(name).") }
    if let text = value as? String, text.isEmpty { throw BridgeError("Missing \(name).") }
    return value
}

func text(_ value: String?, _ name: String) throws -> String {
    let text = try need(value, name)
    guard text.count <= maxText else { throw BridgeError("The \(name) is too long (at most \(maxText) characters).") }
    return text
}

/// Browsers, whose page a look shows as a "web area" with the page's links and text in it.
let browsers: Set<String> = [
    "com.google.Chrome", "com.google.Chrome.beta", "com.google.Chrome.canary", "org.chromium.Chromium",
    "net.imput.helium", "com.brave.Browser", "com.microsoft.edgemac", "company.thebrowser.Browser",
    "company.thebrowser.dia", "com.vivaldi.Vivaldi", "com.operasoftware.Opera", "com.apple.Safari",
    "org.mozilla.firefox",
]

/// How long a look at a browser waits for its page to show. A Chromium browser builds the page for Accessibility
/// only once the bridge turns that on, so the first look after starting can come before it.
let pageWait: TimeInterval = 3

/// Whether a look shows a web page with something in it: a "web area" line with deeper lines under it.
func showsPage(_ text: String) -> Bool {
    let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
    let depth = { (line: Substring) in line.prefix { $0 == "\t" }.count }
    for (index, line) in lines.enumerated() {
        let rest = line.drop { $0 == "\t" }
        guard let space = rest.firstIndex(of: " "), Int(rest[..<space]) != nil, rest[space...].hasPrefix(" web area") else { continue }
        if index + 1 < lines.count, depth(lines[index + 1]) > depth(line) { return true }
    }
    return false
}

/// Whether a window is on the screen the user sees: not minimized, hidden or on another desktop.
func onScreen(_ windowID: Int) -> Bool {
    let info = CGWindowListCopyWindowInfo(.optionIncludingWindow, CGWindowID(windowID)) as? [[String: Any]]
    return info?.first?[kCGWindowIsOnscreen as String] as? Bool ?? false
}

/// A look at an app. A browser window whose page doesn't show is looked at again for a moment; if it still doesn't,
/// the look says why it can't be read, with a screenshot of the window when there's something to see.
func look(app: String, window: String?, screenshot: Bool, client: ComputerUseClient) async throws -> String {
    if screenshot {
        guard CGPreflightScreenCaptureAccess() else { return "\(noScreenshot)\n\n\(try client.getAppState(app: app, windowTitle: window).text)" }
        return try client.getAppState(app: app, windowTitle: window, includeScreenshot: true).text
    }
    var output = try client.getAppState(app: app, windowTitle: window)
    guard browsers.contains(output.metadata?.bundleID ?? ""), output.text.contains("<app_state surface=\"window\">") else { return output.text }
    let deadline = Date().addingTimeInterval(pageWait)
    while !showsPage(output.text), Date() < deadline {
        try await Task.sleep(nanoseconds: 500_000_000)
        output = try client.getAppState(app: app, windowTitle: window)
    }
    if showsPage(output.text) { return output.text }
    // A browser stops drawing the page of a window nobody can see, and after a while stops showing it to
    // Accessibility too; a reload or a switch of tabs didn't bring it back (Helium on another desktop, 2026-10-08).
    guard let id = output.metadata?.windowID, onScreen(id) else {
        let note = "The page in this browser window can't be read or seen: the window isn't on the user's screen (it's minimized, hidden or on another desktop), and a browser stops drawing pages nobody can see. Ask the user to bring the window up on their screen, then look again. A public page can also be read without their browser, with a web search or a browser of your own."
        return "\(note)\n\n\(output.text)"
    }
    var note = "The page in this browser window can't be read: the browser shows its toolbar and tabs but nothing of the page, which happens while a page loads and sometimes after. To read it, look again in a moment, or reload the tab (key cmd+r) and look again."
    // Without Screen Recording, macOS hands back the window without its content.
    if CGPreflightScreenCaptureAccess(), let shot = try? client.getAppState(app: app, windowTitle: window, includeScreenshot: true), let path = shot.metadata?.screenshotPath {
        output = shot
        // Filo's use_computer tool puts what the screenshot shows in place of its path (the last lines).
        note += " A screenshot of the window was taken instead: \(path)."
    } else {
        note += " No screenshot of it either: that needs Screen Recording for Filo (System Settings, Privacy & Security, Screen & System Audio Recording)."
    }
    return "\(note)\n\n\(output.text)"
}

/// Why an action or look asked for a screenshot comes without one.
let noScreenshot = "No screenshot: that needs Screen Recording for Filo (System Settings, Privacy & Security, Screen & System Audio Recording). Ask the user to turn it on, or go by the elements below."

/// An action's result, the window as it is after it, with a screenshot when one was asked for and can be taken.
func acted(_ screenshot: Bool, _ act: (Bool) async throws -> ComputerUseCommandOutput) async throws -> String {
    let allowed = screenshot && CGPreflightScreenCaptureAccess()
    let text = try await act(allowed).text
    return screenshot && !allowed ? "\(noScreenshot)\n\n\(text)" : text
}

func run(_ request: Request, client: ComputerUseClient) async throws -> String {
    let args = request.args
    let shot = args?.screenshot == true
    switch request.command {
    case "apps":
        return client.listApps().text
    case "open":
        return try await client.openApp(need(args?.app, "app")).text
    case "windows":
        return try client.listWindows(app: need(args?.app, "app")).text
    case "look":
        return try await look(app: need(args?.app, "app"), window: args?.window, screenshot: shot, client: client)
    case "click":
        if let element = args?.element {
            return try await acted(shot) { try await client.click(elementIndex: element, includeScreenshotAfter: $0) }
        }
        guard let x = args?.x, let y = args?.y else { throw BridgeError("Missing element (or x and y, a point in the last screenshot).") }
        return try await acted(shot) { try await client.click(x: x, y: y, includeScreenshotAfter: $0) }
    case "drag":
        let (x, y, toX, toY) = try (need(args?.x, "x"), need(args?.y, "y"), need(args?.toX, "toX"), need(args?.toY, "toY"))
        return try await acted(shot) { try await client.drag(fromX: x, fromY: y, toX: toX, toY: toY, includeScreenshotAfter: $0) }
    case "type":
        let typed = try text(args?.text, "text")
        return try await acted(shot) { try await client.typeText(text: typed, elementIndex: args?.element, includeScreenshotAfter: $0) }
    case "set_value":
        let (element, value) = try (need(args?.element, "element"), text(args?.value, "value"))
        return try await acted(shot) { try await client.setValue(elementIndex: element, value: value, includeScreenshotAfter: $0) }
    case "key":
        let key = try need(args?.key, "key")
        return try await acted(shot) { try await client.pressKey(key: key, includeScreenshotAfter: $0) }
    case "scroll":
        let direction = try need(args?.direction, "direction")
        guard ["up", "down", "left", "right"].contains(direction) else { throw BridgeError("direction is up, down, left or right.") }
        let element = try need(args?.element, "element")
        let pages = min(max(args?.pages ?? 1, 0.1), 10)
        return try await acted(shot) { try await client.scroll(elementIndex: element, direction: direction, pages: pages, includeScreenshotAfter: $0) }
    case "action":
        let (element, name) = try (need(args?.element, "element"), need(args?.action, "action"))
        return try await acted(shot) { try await client.performSecondaryAction(elementIndex: element, action: name, includeScreenshotAfter: $0) }
    default:
        throw BridgeError("Unknown command \(request.command).")
    }
}

func write(_ reply: Reply) {
    if let data = try? JSONEncoder().encode(reply), let line = String(data: data, encoding: .utf8) {
        print(line)
    } else {
        print(#"{"id":"","ok":false,"text":"","error":"The bridge couldn't encode its reply."}"#)
    }
    fflush(stdout)
}

/// How long the cursor and border stay after Filo's last call; the next action brings them back.
let restAfter: TimeInterval = 20

/// Takes the cursor and border away once Filo stops acting for a while. Touched on the main queue only.
final class Rest: @unchecked Sendable {
    private var pending: DispatchWorkItem?

    func cancel() {
        DispatchQueue.main.async {
            self.pending?.cancel()
            self.pending = nil
        }
    }

    func schedule(_ effects: AppKitComputerUseVisualEffects) {
        DispatchQueue.main.async {
            self.pending?.cancel()
            let item = DispatchWorkItem { effects.finish() }
            self.pending = item
            DispatchQueue.main.asyncAfter(deadline: .now() + restAfter, execute: item)
        }
    }
}

final class ReplyBox: @unchecked Sendable {
    var reply = Reply(id: "", ok: false, text: "", error: "The bridge didn't finish that request.")
}

func reply(to request: Request, client: ComputerUseClient) -> Reply {
    let box = ReplyBox()
    let done = DispatchSemaphore(value: 0)
    Task.detached {
        do {
            box.reply = Reply(id: request.id, ok: true, text: try await run(request, client: client))
        } catch {
            box.reply = Reply(id: request.id, ok: false, text: "", error: String(describing: error))
        }
        done.signal()
    }
    done.wait()
    return box.reply
}

/// Answers requests in order until stdin closes or the runner says stop.
func serve(client: ComputerUseClient, effects: AppKitComputerUseVisualEffects) {
    let rest = Rest()
    while let line = readLine(strippingNewline: true) {
        if line.trimmingCharacters(in: .whitespaces).isEmpty { continue }
        guard let request = try? JSONDecoder().decode(Request.self, from: Data(line.utf8)) else {
            write(Reply(id: "", ok: false, text: "", error: "The bridge couldn't read that request."))
            continue
        }
        if request.command == "stop" {
            write(Reply(id: request.id, ok: true, text: "Stopped."))
            return
        }
        rest.cancel()
        write(reply(to: request, client: client))
        rest.schedule(effects)
    }
}

@main
struct FiloComputerUse {
    @MainActor
    static func main() {
        let app = NSApplication.shared
        // No Dock icon or menu bar: the bridge only shows the cursor and border, which never take focus.
        app.setActivationPolicy(.accessory)
        let client = ComputerUseClient()
        let effects = AppKitComputerUseVisualEffects()
        client.session.visualEffectHook = effects
        Thread.detachNewThread {
            serve(client: client, effects: effects)
            DispatchQueue.main.async {
                client.finish()
                exit(0)
            }
        }
        app.run()
    }
}
