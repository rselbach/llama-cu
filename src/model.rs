use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A point in screen or window coordinates, measured in points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl FromStr for Point {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (x, y) = s
            .split_once(',')
            .ok_or_else(|| format!("expected X,Y but got {s:?}"))?;
        let parse = |v: &str| {
            v.trim()
                .parse::<f64>()
                .map_err(|_| format!("invalid coordinate {v:?} in {s:?}"))
        };
        Ok(Self {
            x: parse(x)?,
            y: parse(y)?,
        })
    }
}

/// A rectangle with a top-left origin, measured in points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    /// Returns the center point of the rectangle.
    pub fn center(&self) -> Point {
        Point {
            x: self.x + self.width / 2.0,
            y: self.y + self.height / 2.0,
        }
    }

    /// Reports whether the rectangle has a positive area.
    pub fn has_area(&self) -> bool {
        self.width > 0.0 && self.height > 0.0
    }

    /// Reports whether two rectangles overlap.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }

    /// Returns the overlap of two rectangles, if they overlap.
    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right > x && bottom > y).then_some(Rect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        })
    }

    /// Returns the rectangle translated so `origin` becomes (0, 0).
    pub fn relative_to(&self, origin: Point) -> Rect {
        Rect {
            x: self.x - origin.x,
            y: self.y - origin.y,
            ..*self
        }
    }

    /// Returns the top-left corner.
    pub fn origin(&self) -> Point {
        Point {
            x: self.x,
            y: self.y,
        }
    }

    /// Returns the rectangle with every coordinate multiplied by `factor`.
    pub fn scaled(&self, factor: f64) -> Rect {
        Rect {
            x: self.x * factor,
            y: self.y * factor,
            width: self.width * factor,
            height: self.height * factor,
        }
    }
}

/// An installed or running application.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    /// Platform application identifier: bundle ID on macOS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Process ID when the app is running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
}

/// A top-level window that belongs to an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub id: u64,
    pub title: String,
    /// Window frame in screen coordinates.
    pub frame: Rect,
    pub focused: bool,
    pub minimized: bool,
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

/// Platform-neutral description of one accessibility element.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    /// Human-readable role such as "button" or "text field".
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    /// Target of a link, or address of a web page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Frame in screen coordinates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<Rect>,
    pub enabled: bool,
    pub focused: bool,
    pub selected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    /// Whether `set-value` can replace the element's value.
    pub settable: bool,
    /// Accessibility actions the element exposes, such as "press" or "showMenu".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
}

impl NodeInfo {
    /// Returns the best short label for the element.
    pub fn label(&self) -> Option<&str> {
        self.title
            .as_deref()
            .or(self.description.as_deref())
            .filter(|s| !s.is_empty())
    }

    /// Reports whether the element exposes the named action (case-insensitive).
    pub fn has_action(&self, name: &str) -> bool {
        self.actions.iter().any(|a| a.eq_ignore_ascii_case(name))
    }
}

/// One element captured by a platform snapshot, in depth-first order.
#[derive(Debug, Clone)]
pub struct SnapshotNode {
    /// Indentation level in the rendered tree.
    pub depth: usize,
    /// Child indices from the application root, in the order the backend
    /// lists children, used to find the element again.
    pub path: Vec<usize>,
    pub info: NodeInfo,
}

/// Options that bound the size of an accessibility snapshot.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotOptions {
    pub max_nodes: usize,
}

/// The accessibility tree of an app's window, plus its menu bar and open menus.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub nodes: Vec<SnapshotNode>,
    /// True when the tree was cut short by `max_nodes` or the traversal budget.
    pub truncated: bool,
    /// Text selected in the app's focused element, if any.
    pub selected_text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_intersection() {
        let r = |x, y, width, height| Rect {
            x,
            y,
            width,
            height,
        };
        let window = r(0.0, 0.0, 100.0, 100.0);
        let cases = [
            (
                "inside",
                r(10.0, 10.0, 20.0, 20.0),
                Some(r(10.0, 10.0, 20.0, 20.0)),
            ),
            (
                "taller than window",
                r(0.0, -50.0, 80.0, 400.0),
                Some(r(0.0, 0.0, 80.0, 100.0)),
            ),
            ("outside", r(200.0, 0.0, 10.0, 10.0), None),
            ("touching edge", r(100.0, 0.0, 10.0, 10.0), None),
        ];
        for (name, rect, want) in cases {
            assert_eq!(rect.intersection(&window), want, "{name}");
        }
    }

    #[test]
    fn point_parsing() {
        assert_eq!("10, 20.5".parse::<Point>(), Ok(Point { x: 10.0, y: 20.5 }));
        assert!("10".parse::<Point>().is_err());
        assert!("a,b".parse::<Point>().is_err());
    }
}
