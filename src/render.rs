//! Compact text output, designed to be read by agents.

use std::fmt::{self, Write};
use std::str::FromStr;

use crate::commands::{
    Action, AppDetails, AppList, AxState, Capture, Doctor, Observation, Screenshot,
    StateAndScreenshot, StateNode,
};
use crate::model::{AppInfo, Rect, WindowInfo};
use crate::skill::InstalledSkill;

/// Most characters of one text shown in the accessibility tree; `None`
/// shows text in full. Parses from a positive number or `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextLimit(pub Option<usize>);

impl Default for TextLimit {
    fn default() -> Self {
        Self(Some(200))
    }
}

impl FromStr for TextLimit {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("max") {
            return Ok(Self(None));
        }
        match s.parse::<usize>() {
            Ok(n) if n > 0 => Ok(Self(Some(n))),
            _ => Err(format!("expected a positive number or max, got {s:?}")),
        }
    }
}

impl fmt::Display for TextLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(n) => write!(f, "{n}"),
            None => f.write_str("max"),
        }
    }
}

/// Text rendering of a command result.
pub trait Render {
    /// Renders the result as text without a trailing newline.
    fn render(&self) -> String;
}

impl Render for AppList {
    fn render(&self) -> String {
        let width = self
            .apps
            .iter()
            .map(|a| a.app.name.chars().count())
            .max()
            .unwrap_or(0)
            .min(32);
        let lines: Vec<String> = self
            .apps
            .iter()
            .map(|listed| {
                let a = &listed.app;
                let pid = a.pid.map_or("-".to_string(), |p| format!("pid {p}"));
                let id = a.bundle_id.as_deref().or(a.path.as_deref()).unwrap_or("");
                let line = format!("{:<width$}  {pid:<10}  {id}", a.name);
                if listed.frontmost {
                    return format!("{line}  frontmost");
                }
                line
            })
            .collect();
        lines.join("\n")
    }
}

impl Render for AppDetails {
    fn render(&self) -> String {
        let mut out = app_line(&self.app);
        if self.windows.is_empty() {
            out.push_str("\nno windows");
        }
        for w in &self.windows {
            let _ = write!(out, "\nwindow {}", window_line(w));
        }
        out
    }
}

impl Render for AxState {
    fn render(&self) -> String {
        let mut out = app_line(&self.app);
        match &self.window {
            Some(w) => {
                let _ = write!(
                    out,
                    "\nwindow {}\nframes are window-relative (x,y wxh), matching screenshot pixels",
                    window_line(w)
                );
                if self.scale < 1.0 {
                    let _ = write!(
                        out,
                        " (screenshots are scaled to {:.2}x the window's size in points)",
                        self.scale
                    );
                }
            }
            None => out.push_str("\nno window; frames are screen coordinates"),
        }
        for w in &self.other_windows {
            let _ = write!(out, "\nother window {}", window_line(w));
        }
        for node in &self.nodes {
            out.push('\n');
            out.push_str(&node_line(node, self.text_limit));
        }
        if self.truncated {
            out.push_str("\n... truncated; raise --max-nodes or close unrelated UI to see more");
        }
        if let Some(text) = &self.selected_text {
            let _ = write!(
                out,
                "\nselected text: {}",
                quote(&truncate(text, self.text_limit))
            );
        }
        out
    }
}

impl Render for Screenshot {
    fn render(&self) -> String {
        format!(
            "screenshot {} ({}x{}) of window {}",
            self.path.display(),
            self.width,
            self.height,
            window_line(&self.window)
        )
    }
}

impl Render for StateAndScreenshot {
    fn render(&self) -> String {
        let screenshot = match &self.screenshot {
            Capture::Taken(s) => s.render(),
            Capture::Failed(err) => format!("screenshot failed: {err}"),
        };
        format!("{screenshot}\n{}", self.state.render())
    }
}

impl Render for Action {
    fn render(&self) -> String {
        let ok = format!("ok: {}", self.message);
        match &self.observed {
            None => ok,
            Some(Observation::State(state)) => format!("{ok}\n{}", state.render()),
            Some(Observation::Failed(err)) => {
                format!("{ok}\nobserving after the action failed: {err}")
            }
        }
    }
}

impl Render for InstalledSkill {
    fn render(&self) -> String {
        format!("installed the llama-cu skill at {}", self.path.display())
    }
}

impl Render for Doctor {
    fn render(&self) -> String {
        let status = |granted| if granted { "granted" } else { "missing" };
        let p = &self.permissions;
        let mut out = format!(
            "accessibility: {}\nscreen recording: {}",
            status(p.accessibility),
            status(p.screen_recording)
        );
        if p.accessibility && p.screen_recording {
            return out;
        }
        match &p.app {
            Some(app) => {
                let _ = write!(
                    out,
                    "\ngrant the missing permissions to llama-cu ({}) in System Settings > \
                     Privacy & Security; `llama-cu doctor --prompt` opens the system prompts",
                    app.display()
                );
            }
            None => out.push_str(
                "\ngrant the missing permissions to the app that runs llama-cu (for example your terminal) \
                 in System Settings > Privacy & Security, then restart that app; \
                 `llama-cu doctor --prompt` opens the system prompts",
            ),
        }
        out
    }
}

fn app_line(app: &AppInfo) -> String {
    let mut out = app.name.clone();
    if let Some(id) = &app.bundle_id {
        let _ = write!(out, " ({id})");
    }
    if let Some(pid) = app.pid {
        let _ = write!(out, " pid {pid}");
    }
    out
}

fn window_line(w: &WindowInfo) -> String {
    let mut out = format!(
        "{} {} {}x{} at {},{}",
        w.id,
        quote(&w.title),
        w.frame.width.round(),
        w.frame.height.round(),
        w.frame.x.round(),
        w.frame.y.round()
    );
    if w.focused {
        out.push_str(" focused");
    }
    if w.minimized {
        out.push_str(" minimized");
    }
    out
}

fn node_line(node: &StateNode, limit: TextLimit) -> String {
    let info = &node.info;
    let mut out = format!("{}[{}] {}", "  ".repeat(node.depth), node.id, info.role);
    // Static text carries its content in the value.
    let label = info
        .label()
        .or_else(|| info.value.as_deref().filter(|_| info.role == "text"));
    match (label, info.url.as_deref()) {
        (Some(label), Some(url)) if info.role == "link" => {
            let label = markdown_text(&truncate(label, limit));
            let _ = write!(out, " [{label}]({})", truncate(url, limit));
        }
        (None, Some(url)) if info.role == "link" => {
            let _ = write!(out, " <{}>", truncate(url, limit));
        }
        (Some(label), url) => {
            let _ = write!(out, " {}", quote(&truncate(label, limit)));
            if let Some(url) = url {
                let _ = write!(out, " url={}", quote(&truncate(url, limit)));
            }
        }
        (None, Some(url)) => {
            let _ = write!(out, " url={}", quote(&truncate(url, limit)));
        }
        (None, None) => {}
    }
    if let (Some(_), Some(desc)) = (&info.title, &info.description)
        && Some(desc.as_str()) != label
    {
        let _ = write!(out, " desc={}", quote(desc));
    }
    // An empty value says something only about text inputs.
    let input = ["text field", "text area", "search field", "combo box"]
        .iter()
        .any(|suffix| info.role.ends_with(suffix));
    if let Some(value) = info
        .value
        .as_deref()
        .filter(|v| Some(*v) != label && (input || !v.is_empty()))
    {
        let _ = write!(out, " value={}", quote(&truncate(value, limit)));
    }
    if let Some(placeholder) = &info.placeholder {
        let _ = write!(out, " placeholder={}", quote(placeholder));
    }
    if let Some(frame) = &info.frame {
        let _ = write!(out, " {}", frame_text(frame));
    }
    // Disabled and expanded state matter only for elements you can act on.
    let actionable = info.actions.iter().any(|a| a != "scrollToVisible");
    let mut flags = Vec::new();
    if info.focused {
        flags.push("focused");
    }
    if info.selected {
        flags.push("selected");
    }
    if actionable && !info.enabled {
        flags.push("disabled");
    }
    if info.settable {
        flags.push("settable");
    }
    match info.expanded.filter(|_| actionable) {
        Some(true) => flags.push("expanded"),
        Some(false) => flags.push("collapsed"),
        None => {}
    }
    match info.checked {
        Some(true) => flags.push("checked"),
        Some(false) => flags.push("unchecked"),
        None => {}
    }
    for flag in flags {
        out.push(' ');
        out.push_str(flag);
    }
    let actions: Vec<&str> = info
        .actions
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "scrollToVisible")
        .collect();
    if !actions.is_empty() {
        let _ = write!(out, " actions={}", actions.join(","));
    }
    out
}

fn frame_text(f: &Rect) -> String {
    format!(
        "({},{} {}x{})",
        f.x.round(),
        f.y.round(),
        f.width.round(),
        f.height.round()
    )
}

fn truncate(s: &str, limit: TextLimit) -> String {
    let count = s.chars().count();
    let Some(max) = limit.0.filter(|&max| count > max) else {
        return s.to_string();
    };
    let head: String = s.chars().take(max).collect();
    format!("{head}... ({count} chars)")
}

/// Escapes text for the label of a Markdown link and keeps it on one line.
fn markdown_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '[' | ']' => {
                out.push('\\');
                out.push(c);
            }
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeInfo;

    #[test]
    fn node_lines() {
        let frame = Some(Rect {
            x: 10.4,
            y: 20.0,
            width: 80.0,
            height: 22.0,
        });
        let cases = [
            (
                "button",
                NodeInfo {
                    role: "button".into(),
                    title: Some("Save".into()),
                    frame,
                    enabled: true,
                    actions: vec!["press".into(), "scrollToVisible".into()],
                    ..NodeInfo::default()
                },
                1,
                r#"  [7] button "Save" (10,20 80x22) actions=press"#,
            ),
            (
                "text field",
                NodeInfo {
                    role: "text field".into(),
                    description: Some("Name".into()),
                    value: Some("Troy \"T\" Barnes\n".into()),
                    enabled: true,
                    focused: true,
                    ..NodeInfo::default()
                },
                0,
                r#"[7] text field "Name" value="Troy \"T\" Barnes\n" focused"#,
            ),
            (
                "disabled checkbox",
                NodeInfo {
                    role: "check box".into(),
                    title: Some("Remember".into()),
                    checked: Some(false),
                    actions: vec!["press".into()],
                    ..NodeInfo::default()
                },
                0,
                r#"[7] check box "Remember" disabled unchecked actions=press"#,
            ),
            (
                "static text shows its value as the label",
                NodeInfo {
                    role: "text".into(),
                    value: Some("Troy Barnes".into()),
                    enabled: false,
                    expanded: Some(false),
                    ..NodeInfo::default()
                },
                0,
                r#"[7] text "Troy Barnes""#,
            ),
            (
                "static text value equals label",
                NodeInfo {
                    role: "text".into(),
                    description: Some("Greendale".into()),
                    value: Some("Greendale".into()),
                    enabled: true,
                    ..NodeInfo::default()
                },
                0,
                r#"[7] text "Greendale""#,
            ),
            (
                "link with url renders as markdown",
                NodeInfo {
                    role: "link".into(),
                    description: Some("Study [Group]".into()),
                    url: Some("https://greendale.edu/study".into()),
                    enabled: true,
                    actions: vec!["press".into()],
                    ..NodeInfo::default()
                },
                0,
                r"[7] link [Study \[Group\]](https://greendale.edu/study) actions=press",
            ),
            (
                "unlabeled link shows its url",
                NodeInfo {
                    role: "link".into(),
                    url: Some("https://greendale.edu".into()),
                    enabled: true,
                    ..NodeInfo::default()
                },
                0,
                r"[7] link <https://greendale.edu>",
            ),
            (
                "web area shows its address",
                NodeInfo {
                    role: "web area".into(),
                    title: Some("Greendale".into()),
                    url: Some("https://greendale.edu".into()),
                    enabled: true,
                    ..NodeInfo::default()
                },
                0,
                r#"[7] web area "Greendale" url="https://greendale.edu""#,
            ),
            (
                "settable field",
                NodeInfo {
                    role: "text field".into(),
                    value: Some(String::new()),
                    enabled: true,
                    settable: true,
                    ..NodeInfo::default()
                },
                0,
                r#"[7] text field value="" settable"#,
            ),
        ];
        for (name, info, depth, want) in cases {
            let node = StateNode { id: 7, depth, info };
            assert_eq!(node_line(&node, TextLimit::default()), want, "{name}");
        }
    }

    #[test]
    fn text_limits() {
        let long = "x".repeat(205);
        let cases = [
            (
                "default",
                TextLimit::default(),
                "x".repeat(200) + "... (205 chars)",
            ),
            (
                "custom",
                TextLimit(Some(3)),
                "xxx... (205 chars)".to_string(),
            ),
            ("max", TextLimit(None), long.clone()),
        ];
        for (name, limit, want) in cases {
            assert_eq!(truncate(&long, limit), want, "{name}");
        }
    }

    #[test]
    fn text_limit_parsing() {
        assert_eq!("max".parse(), Ok(TextLimit(None)));
        assert_eq!("MAX".parse(), Ok(TextLimit(None)));
        assert_eq!("50".parse(), Ok(TextLimit(Some(50))));
        assert!("0".parse::<TextLimit>().is_err());
        assert!("all".parse::<TextLimit>().is_err());
    }
}
