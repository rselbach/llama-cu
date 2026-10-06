// A disposable native app for check-background.py. No activation or real input.
import AppKit
import CoreGraphics

final class ProbeView: NSView {
  var events: [[String: Any]] = []
  override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

  func record(_ event: NSEvent) {
    let point = convert(event.locationInWindow, from: nil)
    events.append([
      "type": event.type.rawValue, "count": event.type == .scrollWheel ? 0 : event.clickCount,
      "point": [point.x, point.y],
      "delta": event.type == .scrollWheel
        ? [event.scrollingDeltaX, event.scrollingDeltaY] : [0, 0],
    ])
  }

  override func mouseDown(with event: NSEvent) { record(event) }
  override func mouseUp(with event: NSEvent) { record(event) }
  override func rightMouseDown(with event: NSEvent) { record(event) }
  override func rightMouseUp(with event: NSEvent) { record(event) }
  override func otherMouseDown(with event: NSEvent) { record(event) }
  override func otherMouseUp(with event: NSEvent) { record(event) }
  override func mouseDragged(with event: NSEvent) { record(event) }
  override func scrollWheel(with event: NSEvent) { record(event) }
}

final class Panel: NSObject {
  let window: NSWindow
  let view = ProbeView(frame: NSRect(x: 0, y: 0, width: 500, height: 350))
  let text = NSTextField(frame: NSRect(x: 30, y: 250, width: 400, height: 30))
  var presses = 0

  init(title: String, x: CGFloat) {
    window = NSWindow(
      contentRect: NSRect(x: x, y: 100, width: 500, height: 350),
      styleMask: [.titled, .closable, .miniaturizable], backing: .buffered, defer: false)
    super.init()
    window.title = title
    window.isReleasedWhenClosed = false
    text.placeholderString = "Troy Barnes"
    view.addSubview(text)
    let button = NSButton(title: "Greendale", target: self, action: #selector(press))
    button.frame = NSRect(x: 30, y: 290, width: 130, height: 30)
    view.addSubview(button)
    window.contentView = view
    window.orderBack(nil)
    window.makeFirstResponder(text)
  }

  @objc func press() { presses += 1 }

  var state: [String: Any] {
    [
      "window": window.windowNumber, "title": window.title,
      "events": view.events, "text": text.stringValue, "presses": presses,
    ]
  }
}

final class Delegate: NSObject, NSApplicationDelegate {
  var panels: [Panel] = []
  var timer: Timer?
  var everActive = false
  var samples: [[String: Any]] = []
  let output = Bundle.main.bundleURL.deletingLastPathComponent()
    .appendingPathComponent("state.json")

  func applicationDidFinishLaunching(_ notification: Notification) {
    let menu = NSMenu()
    let edit = NSMenuItem(title: "Edit", action: nil, keyEquivalent: "")
    edit.submenu = NSMenu(title: "Edit")
    edit.submenu?.addItem(
      NSMenuItem(title: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a"))
    menu.addItem(edit)
    NSApp.mainMenu = menu
    panels = [Panel(title: "Greendale One", x: 100), Panel(title: "Greendale Two", x: 650)]
    panels[0].window.makeKey()
    panels[0].window.makeFirstResponder(panels[0].text)
    timer = Timer.scheduledTimer(withTimeInterval: 0.01, repeats: true) { [self] _ in
      writeState()
    }
  }

  func writeState() {
    everActive = everActive || NSApp.isActive
    guard let event = CGEvent(source: nil) else {
      fputs("cannot read pointer location\n", stderr)
      NSApp.terminate(nil)
      return
    }
    samples.append([
      "time": Date().timeIntervalSince1970,
      "pointer": [event.location.x, event.location.y],
      "front": NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0,
    ])
    if samples.count > 500 { samples.removeFirst(samples.count - 500) }
    let state: [String: Any] = [
      "pid": ProcessInfo.processInfo.processIdentifier, "everActive": everActive,
      "panels": panels.map(\.state), "samples": samples,
    ]
    do {
      let data = try JSONSerialization.data(withJSONObject: state)
      try data.write(to: output, options: .atomic)
    } catch {
      fputs("probe state: \(error)\n", stderr)
      NSApp.terminate(nil)
    }
  }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let delegate = Delegate()
app.delegate = delegate
app.run()
