# llama-cu

`llama-cu` is a command-line tool that lets agents operate desktop apps. It
reads accessibility trees, captures windows, and sends mouse, keyboard, and
clipboard input. It is built for [Pi](https://pi.dev) skills, but any agent
that can run shell commands can use it.

The current version supports macOS 14 and later. The code is organized so
that Linux and Windows backends can be added without changing the commands.

## Install

```sh
just install        # cargo install --path .
just install-skill  # link skills/llama-cu into ~/.agents/skills for Pi
```

macOS asks for two permissions, and both go to the app that runs
`llama-cu`, usually your terminal or agent host:

- **Accessibility**: read the element tree, perform actions, and send input.
- **Screen Recording**: capture windows.

Run `llama-cu doctor` to check them, or `llama-cu doctor --prompt` to open
the system prompts. Restart the host app after you grant Screen Recording.

## Usage

```sh
llama-cu get-app TextEdit                 # select and launch the app
llama-cu get-ax-state-and-screenshot      # element tree plus a PNG path
llama-cu click --element 14               # act on an element ID
llama-cu type-text "Greendale Community College"
llama-cu press-key cmd+s
```

Run `llama-cu --help` or `llama-cu <command> --help` for every option. The
[skill](skills/llama-cu/SKILL.md) is the agent-facing guide.

| Command | Capability |
|---|---|
| `list-apps` | List apps and whether they are running. |
| `get-app` | Select an app by name, path, or bundle ID; launch it if needed. |
| `get-ax-state` | Read the app's accessibility tree: controls, text, and actions. |
| `get-screenshot` | Capture the app window. |
| `get-ax-state-and-screenshot` | Do both. |
| `click` | Click an element or coordinates, with any button and click count. |
| `drag` | Drag between coordinates. |
| `scroll` | Scroll at an element or coordinates. |
| `press-key` | Send keys and shortcuts. |
| `type-text` | Type into the focused control. |
| `paste` | Paste plain text, Markdown, or HTML, then restore the clipboard. |
| `set-value` | Replace an editable control's value. |
| `select-text` | Select matching text or place the cursor before or after it. |
| `perform-secondary-action` | Invoke an exposed accessibility action. |

Output is compact text by default. With `--json`, results go to standard
output as JSON, and errors go to standard error as
`{"error":{"code":"...","message":"..."}}`. The exit status is 1 on error.

The text tree shows at most 200 characters of each text. Use
`--text-limit N` or `--text-limit max` to read longer text; JSON output is
never cut. Add `--observe` to an action command to print the app's state and
a screenshot after the action, which saves a separate `get-ax-state` call.
If a screenshot fails, `get-ax-state-and-screenshot` still prints the tree
and reports the failure.

`llama-cu` refuses to operate password managers, such as 1Password,
Bitwarden, Keychain Access, and Apple Passwords. `get-app` fails with
`app_blocked` and does not launch them.

### Sessions and element IDs

Every invocation is a separate process. State that must survive between
calls lives in a session file: the selected app, the last observed window,
and the element IDs from the last `get-ax-state`. Session files and
screenshots are stored in `$LLAMA_CU_STATE_DIR`, else
`$XDG_RUNTIME_DIR/llama-cu`, else the per-user temporary directory. Set
`LLAMA_CU_SESSION` (or `--session`) to keep concurrent agents apart.

An element ID records the element's child-index path from the application
root and a fingerprint (role, title, description, and identifier). Elements
with none of these are fingerprinted by their window-relative frame. Before
acting, `llama-cu` follows the path again and checks the fingerprint. If the
element changed, the command fails with `stale_element` and does not act on
the wrong element.

### Coordinates

Coordinates in `--at`, `--from`, and `--to`, and frames in the tree, are
screenshot pixels relative to the top-left corner of the last observed
window, so they stay valid if the window moves.

Screenshots are captured at one pixel per point, except for windows larger
than 1280 pixels on a side or about 1.15 megapixels. Model APIs shrink images
that large before the model sees them, which would break the match between
what the model sees and the coordinates it sends. `llama-cu` captures those
windows scaled down, scales the tree's frames the same way, and converts
coordinates back to points. The tree header says when a window is scaled.

## Architecture

```
src/
  main.rs         CLI definition and output
  commands.rs     command behavior shared by all platforms
  session.rs      session state and element fingerprints
  render.rs       text output
  keys.rs         key combo parsing
  text.rs         Markdown to HTML and HTML to text for paste
  platform/
    mod.rs        Platform trait
    macos/        AX API, ScreenCaptureKit, CGEvent, NSPasteboard
```

`platform::Platform` is the boundary. A backend provides thin primitives:
list apps, launch, list windows, activate, snapshot the tree, resolve a
path, read or act on an element, capture a window, send input, and save,
set, or restore the clipboard. Behavior such as app matching, window choice,
element verification, the choice between press and pointer click, and paste
sequencing lives in `commands.rs`, so each platform behaves the same.

To add a platform, implement `Platform` in `platform/<os>/` and select it in
`platform::current()`:

- **Windows**: UI Automation through the `windows` crate (tree, patterns
  for actions and values, TextPattern for selection), `SendInput`,
  Windows.Graphics.Capture, and the Win32 clipboard.
- **Linux**: AT-SPI2 over D-Bus (`atspi`/`zbus`) for the tree, actions,
  values, and text selection. Input and capture depend on the display server:
  XTest and XGetImage on X11, or the RemoteDesktop and ScreenCast portals on
  Wayland.

## Development

```sh
just check   # format check, clippy, tests
just build   # release build
```

Tests cover the platform-neutral logic and macOS key mapping. Accessibility,
capture, and input need the permissions above, so test them by hand against
a real app.

## Known limitations

- `select-text` uses `AXSelectedTextRange`. Web content in Safari and
  Chromium uses text markers instead, so selection there is not supported
  yet.
- `paste` waits 500 ms before it restores the clipboard. A very slow app
  might paste the restored contents instead.
- `press-key` maps characters through the current ASCII-capable keyboard
  layout. Characters that no key produces need `type-text`.
- Permissions belong to the host app, so every process that host starts can
  also control the desktop.
- Finder answers the `showMenu` action only after the menu closes, so
  `perform-secondary-action showMenu` times out there. Use
  `click --button right` instead.
- Firefox and some Electron apps, such as Signal, hide their content until an
  accessibility client sets `AXEnhancedUserInterface`. `llama-cu` sets it only
  when a window looks empty. The flag stays on until the app quits, and it
  can slow down window managers' animations for that app.
