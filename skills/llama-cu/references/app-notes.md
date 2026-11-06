# App behavior and limitations

## Window visibility and focus

Covered windows can be captured without bringing them forward. Pointer
input requires an unminimized window on the current Space in an unhidden
app. The blue agent cursor is a separate visual overlay; neither it nor
the real pointer appears in window screenshots.

Background keys go to the app's internally focused window and control.
This is separate from which app is frontmost. Typing stops if the selected
window loses focus, but a slow app may receive some characters after a
dialog opens. Observe and select the intended window before continuing.

An app can activate itself in response to an action. Separate llama-cu
sessions do not isolate app focus, documents, or clipboard effects. Copy
menu actions can change the clipboard even though `paste` is blocked.

## Native controls and menus

Many AppKit views decline clicks while inactive. Prefer an exposed `press`
or `open` action; repeating a coordinate click may still do nothing.
`set-value` can change a field without triggering the events the app uses
to validate or save it. Verify the downstream result and use typing when
the app requires keyboard input.

For menu operations, click the menu bar element, observe the exposed menu
items, and invoke the intended item's accessibility action. Pointer input
outside the selected window is unsupported, so do not use coordinates
from an external menu or popover. Use `escape` to dismiss a menu only when
the selected window still has the app's keyboard focus.

Finder's `showMenu` action can time out while the menu is open. Observe
before retrying; when a context menu is needed, a right click within the
selected window avoids that blocking AX action.

## Browsers and Electron

The first tree read can take up to two seconds while accessibility initializes.
If a web area is empty, observe again before treating it as inaccessible.

Chromium coordinate-click support depends on the version and view. In the
Electron 45.2.3 / Chromium 156.0.8078.80 fixture, AX button presses, AX text
edits, coordinate clicks, scrolling, and screenshots worked in the
background. Drag movement reached the page with DOM `buttons=0`, so
handlers requiring a held button did not perform a drag.

For a web control with this limitation, use an exposed semantic action or
an available browser automation API against the intended test instance.
Do not assume another Electron app behaves identically to the fixture or
that a successful event post means the page handled it.

`select-text` uses `AXSelectedTextRange`; Safari and Chromium web content
can require text markers instead. Prefer an editable `set-value` or the
available browser automation tool when selection is unsupported.

## Permissions

The signed `llama-cu.app` normally owns Accessibility and Screen Recording
permissions. A standalone build can inherit them from the terminal or
agent host. Use the app identity reported by `doctor`; do not guess.
When Screen Recording belongs to the terminal, restarting that terminal
may be required after the user grants it. Recheck `doctor` before retrying.
