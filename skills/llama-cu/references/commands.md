# Command details

Prefix commands below with `llama-cu --background --session NAME`, using
the same task-specific session throughout. IDs and coordinates come from
the latest observation. All mutation commands accept `--observe`.

## Observation and output

| Command | Use |
|---|---|
| `list-apps --running` | Inspect running app identities and PIDs. |
| `get-app <name, bundle ID, or path>` | Select or launch an app and list windows; resets the session's previous targets. |
| `get-ax-state-and-screenshot [--window ID]` | Read a tree and capture the same window. Open the returned PNG. |
| `get-ax-state [--window ID]` | Refresh element IDs without capturing an image. |
| `get-screenshot [--window ID] [-o FILE]` | Capture the window without refreshing the tree's IDs. Observe again before using IDs from a different window. |

Tree lines contain `[id] role`, a label, optional value and frame, state
flags, and actions. For example:

```text
[14] text field "Student" value="Troy Barnes" (30,60 300x30) focused settable
[15] button "Enroll" (30,110 100x30) actions=press
```

`focused` identifies the app's focused control. `settable` means `set-value`
can replace its value. Action names can be passed to
`perform-secondary-action --element ID ACTION`.

Text output limits values to 200 characters. Use `--text-limit N` or
`--text-limit max` to inspect longer text. The tree defaults to 1000 nodes;
increase `--max-nodes N` when `truncated` hides the needed control.

With `--json`, results go to stdout and command errors go to stderr as
`{"error":{"code":"...","message":"..."}}` with exit status 1.
Combined observations contain `state` and either `screenshot` or
`screenshotError`. Actions with `--observe` contain either `observed` or
`observeError`; these observation failures do not mean the action failed.
`state.otherWindows` lists other windows. JSON values are not text-truncated,
but the node limit still applies.

## Input choices

| Command | Behavior |
|---|---|
| `click --element ID` | Uses the control's `press` action when exposed, otherwise pointer input. |
| `click --element ID --count 2` | Uses `open` when exposed, otherwise a double-click. |
| `click --at X,Y [--button right]` | Click at window-relative screenshot coordinates. |
| `scroll [--element ID \| --at X,Y] --direction down --amount 5` | Scroll at the control, point, or window center; also supports up, left, right. |
| `drag --from X,Y --to X,Y` | Drag within the selected window; verify the resulting control state. |
| `set-value --element ID "Troy Barnes"` | Replace an editable value directly. Verify that the app notices the change. |
| `type-text "Troy Barnes"` | Type into the app's focused control. Newlines and tabs act as Return and Tab. |
| `select-text --element ID "Troy" [--position select\|before\|after] [--occurrence N]` | Select a match or place the text cursor. Defaults to the first match. |
| `press-key tab` or `press-key shift+left backspace` | Send plain keys or supported combinations in order. |
| `perform-secondary-action --element ID ACTION` | Invoke an action listed on the element. |

For long literal text, pass `-` to `type-text` or `set-value` and provide
stdin through the host's execution API or a quoted heredoc. Preserve literal
quotes, dollar signs, and backticks rather than treating them as shell code.

Modifiers use `+`: `shift`, `ctrl`, `alt`/`option`, `fn`, and `cmd`.
Command combinations are rejected in background mode. Common named keys
include `enter`, `tab`, `escape`, `space`, `backspace`, `delete`, arrows,
`home`, `end`, `pageup`, `pagedown`, and `f1` through `f20`. Letters ignore
case; use `shift` explicitly for uppercase shortcuts. Prefer `type-text`
for text that cannot be produced by the keyboard layout.

`paste`, Command shortcuts, and the `raise` action require foreground mode.
If the user has authorized desktop takeover, replace `--background` with
`--foreground` for the needed invocation; the flags cannot be combined.
`paste [--format plain|markdown|html]` temporarily replaces the shared clipboard.
