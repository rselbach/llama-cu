# llama-cu

`llama-cu` is a command-line tool that lets agents operate desktop apps. It
reads accessibility trees, captures windows, and sends mouse, keyboard, and
clipboard input. It is built for [Pi](https://pi.dev) skills, but any agent
that can run shell commands can use it.

The current version supports macOS 14 and later. The code is organized so
that Linux and Windows backends can be added without changing the commands.

## Install

```sh
just install        # build and sign llama-cu.app, install it, link the command
just install-skill  # link skills/llama-cu into ~/.agents/skills for Pi
```

`just install` builds `llama-cu.app`, copies it to `~/Applications`, and
links `~/.cargo/bin/llama-cu` to the command inside it. macOS asks for two
permissions, and both go to llama-cu.app:

- **Accessibility**: read the element tree, perform actions, and send input.
- **Screen Recording**: capture windows.

Run `llama-cu doctor` to check them, or `llama-cu doctor --prompt` to open
the system prompts.

macOS normally charges permissions to the app that started a command, such
as your terminal or agent host, and then every program that app runs gets
the same access. To avoid that, the command re-runs itself from inside
llama-cu.app and tells macOS that the app is responsible for itself. This
uses `responsibility_spawnattrs_setdisclaim`, a private but long-stable
macOS function. A binary run from outside the app, such as `cargo run` or
`target/release/llama-cu`, still uses the permissions of the app that
started it.

`just app` builds and signs `target/release/llama-cu.app` without installing
it. It signs with `$LLAMA_CU_SIGN_IDENTITY`, else the first Developer ID
Application identity in your keychain, else ad hoc. macOS keeps the
permissions across rebuilds only while the signing identity stays the same,
so an ad hoc build must be granted again after every rebuild.

### Install a release

Each [release](https://github.com/rselbach/llama-cu/releases) has a signed
and notarized `llama-cu-<version>-macos-arm64.zip`. Unzip it, move
`llama-cu.app` to `~/Applications`, and link the command from a directory
on your `PATH`, for example:

```sh
ln -sfn ~/Applications/llama-cu.app/Contents/MacOS/llama-cu ~/.local/bin/llama-cu
```

Releases are signed with the same Developer ID, so macOS keeps the
permissions when you replace the app with a newer release.

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

### Experimental background input

Use `--background` on each command, or set `LLAMA_CU_BACKGROUND=true` for
the agent's environment:

```sh
export LLAMA_CU_BACKGROUND=true
llama-cu get-app TextEdit
llama-cu get-ax-state-and-screenshot
llama-cu click --element 14
llama-cu type-text "Greendale Community College"
```

This mode sends input directly to the selected process and window, without
explicitly activating the app, raising its window, or moving the real
pointer. It keeps accessibility actions as the first choice. There is no
visible agent cursor overlay yet. The default foreground mode is unchanged.

Background delivery is experimental and app-dependent. Observe after each
action: posting an event successfully does not prove the app handled it.
An app can also activate itself in response to an action. In particular,
browser/Electron content and games need further testing.

- Observe the intended window first. Background keyboard events require it
  to be the app's focused window; otherwise they fail with
  `background_unavailable`. This is focus *within the target app*, separate
  from which app is frontmost.
- Pointer input is limited to the selected, non-minimized window. Menus and
  popovers outside it, other Spaces, and simultaneous work in the same app
  are not supported by this prototype. A missing selected window fails
  instead of redirecting input to another window.
- `paste` and the `raise` action fail in background mode. Use `type-text` or
  `set-value` to avoid replacing the shared clipboard. Other app actions
  such as a Copy menu action can still change the clipboard.
- Command shortcuts are rejected: native apps can silently ignore them
  while inactive. Use accessibility actions on menu items, `select-text`,
  or `set-value` instead. Plain keys and Shift combinations use the app's
  existing keyboard focus.
- There is no automatic fallback to foreground input. To use the original
  behavior, omit the flag and unset `LLAMA_CU_BACKGROUND`.

Mouse delivery uses `CGEventPostToPid`, AppKit event construction, window
metadata, and the private `CGEventSetWindowLocation` function. If that
function is missing, pointer input fails with `background_unavailable`.
This mechanism needs live regression testing when macOS changes. The
[axcli implementation](https://github.com/andelf/axcli/blob/main/src/input.rs)
is a useful reference for the event metadata.

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
    macos/        AX API, ScreenCaptureKit, CGEvent, NSPasteboard, and the
                  hand-off to llama-cu.app
scripts/
  build-app.sh    build and sign llama-cu.app
  notarize-app.sh notarize, staple, and zip llama-cu.app
assets/
  AppIcon.png     app icon, 1024 pixels, in the macOS icon shape
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
just check             # format check, clippy, tests
just build             # release build
just check-background  # live background-input check against a disposable app
```

Tests cover the platform-neutral logic and macOS key mapping. Accessibility,
capture, and input need the permissions above, so test them by hand against
a real app.

`just check-background` needs Python 3, Xcode command-line tools, and the
same Accessibility permission as `cargo run`. It builds a temporary AppKit
app and exercises actual input delivery while
sampling the real pointer and foreground app. Keep the pointer still and
avoid switching apps during this short check; human input also triggers
its interference assertions. The check quits the fixture and removes its
temporary app and session on exit.

## Releases

The Release workflow builds `llama-cu.app` on a macOS runner, signs it with
the Developer ID Application certificate, notarizes it, and uploads the
zip. It runs for pull requests that change packaging, for manual runs, and
for version tags. A tag also publishes a GitHub release.

To release, set `version` in `Cargo.toml`, merge it, then push a matching
tag:

```sh
git tag v0.2.0
git push origin v0.2.0
```

The workflow uses these repository secrets:

| Secret | Contents |
| --- | --- |
| `MACOS_CERTIFICATE_P12_BASE64` | Developer ID Application certificate and key, as base64 `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | Password of the `.p12` |
| `APPLE_SIGNING_IDENTITY` | Full identity name, such as `Developer ID Application: Name (TEAMID)` |
| `APPLE_ID` | Apple Account that submits for notarization |
| `APPLE_APP_SPECIFIC_PASSWORD` | App-specific password of that account |
| `APPLE_TEAM_ID` | Team ID of the certificate |

To notarize locally, set `APPLE_ID`, `APPLE_TEAM_ID`, and
`APPLE_APP_SPECIFIC_PASSWORD`, then run `just notarize`. It builds and signs
the app, refuses an app that another team signed, waits for Apple, staples
the ticket, checks it with Gatekeeper, and writes
`target/release/llama-cu-<version>-macos-<arch>.zip`.

## Known limitations

- `select-text` uses `AXSelectedTextRange`. Web content in Safari and
  Chromium uses text markers instead, so selection there is not supported
  yet.
- `paste` waits 500 ms before it restores the clipboard. A very slow app
  might paste the restored contents instead.
- `press-key` maps characters through the current ASCII-capable keyboard
  layout. Characters that no key produces need `type-text`.
- Finder answers the `showMenu` action only after the menu closes, so
  `perform-secondary-action showMenu` times out there. Use
  `click --button right` instead.
- Firefox and some Electron apps, such as Signal, hide their content until an
  accessibility client sets `AXEnhancedUserInterface`. `llama-cu` sets it only
  when a window looks empty. The flag stays on until the app quits, and it
  can slow down window managers' animations for that app.
