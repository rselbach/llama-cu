use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::model::{AppInfo, NodeInfo, Point, Rect};

/// State that persists between invocations of the same session.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// App selected by the last `get-app`.
    pub app: Option<AppInfo>,
    /// Window from the last state or screenshot; coordinates are relative to it.
    pub window: Option<u64>,
    /// Screenshot pixels per point for `window`; coordinates are in pixels.
    pub scale: Option<f64>,
    /// Element IDs from the last `get-ax-state`.
    pub snapshot: Option<SnapshotRefs>,
}

/// Element references captured by the last snapshot. Element ID `n` is
/// `elements[n - 1]`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRefs {
    pub pid: i32,
    pub window: Option<u64>,
    pub elements: Vec<ElementRef>,
}

/// How to find an element again and confirm it is still the same element.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementRef {
    pub path: Vec<usize>,
    pub fingerprint: Fingerprint,
}

/// Identifying attributes of an element. Elements without a title,
/// description, or identifier are identified by their window-relative frame
/// instead, so a scrolled list cannot silently swap one anonymous row for
/// another.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    pub role: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub identifier: Option<String>,
    pub frame: Option<Rect>,
}

impl Fingerprint {
    /// Captures the fingerprint of an element. `window_origin` is the
    /// screen position of the window the frame is relative to.
    pub fn of(info: &NodeInfo, window_origin: Option<Point>) -> Self {
        let anonymous =
            info.title.is_none() && info.description.is_none() && info.identifier.is_none();
        let frame = match (anonymous, info.frame, window_origin) {
            (true, Some(frame), Some(origin)) => Some(frame.relative_to(origin)),
            _ => None,
        };
        Self {
            role: info.role.clone(),
            title: info.title.clone(),
            description: info.description.clone(),
            identifier: info.identifier.clone(),
            frame,
        }
    }

    /// Reports whether `info` still describes the fingerprinted element.
    pub fn matches(&self, info: &NodeInfo, window_origin: Option<Point>) -> bool {
        let same_frame = match (self.frame, info.frame, window_origin) {
            (None, _, _) => true,
            (Some(want), Some(got), Some(origin)) => {
                let got = got.relative_to(origin);
                (want.x - got.x).abs() < 1.0 && (want.y - got.y).abs() < 1.0
            }
            _ => false,
        };
        same_frame
            && self.role == info.role
            && self.title == info.title
            && self.description == info.description
            && self.identifier == info.identifier
    }
}

/// Loads and saves one named session.
pub struct Store {
    dir: PathBuf,
    name: String,
}

impl Store {
    /// Opens the store for the named session.
    pub fn open(name: &str) -> Result<Self> {
        let valid = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                format!("invalid session name {name:?}: use letters, digits, '-', or '_'"),
            ));
        }
        let dir = state_dir();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            name: name.to_string(),
        })
    }

    /// Loads the session, or returns an empty one if none was saved.
    pub fn load(&self) -> Result<Session> {
        match fs::read(self.session_path()) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Session::default()),
            Err(err) => Err(err.into()),
        }
    }

    /// Saves the session atomically.
    pub fn save(&self, session: &Session) -> Result<()> {
        let path = self.session_path();
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(session)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Returns a fresh path for a screenshot in this session.
    pub fn screenshot_path(&self) -> Result<PathBuf> {
        let dir = self.dir.join("screenshots");
        fs::create_dir_all(&dir)?;
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        Ok(dir.join(format!("{}-{millis}.png", self.name)))
    }

    fn session_path(&self) -> PathBuf {
        self.dir.join(format!("{}.json", self.name))
    }
}

/// Directory for session state and screenshots: `LLAMA_CU_STATE_DIR`, else
/// `$XDG_RUNTIME_DIR/llama-cu`, else the per-user temporary directory.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LLAMA_CU_STATE_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    Path::new(&base).join("llama-cu")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(title: Option<&str>, frame: Rect) -> NodeInfo {
        NodeInfo {
            role: "button".into(),
            title: title.map(String::from),
            frame: Some(frame),
            ..NodeInfo::default()
        }
    }

    #[test]
    fn fingerprint_matching() {
        let frame = Rect {
            x: 110.0,
            y: 220.0,
            width: 50.0,
            height: 20.0,
        };
        let moved = Rect { y: 260.0, ..frame };
        let origin = Point { x: 100.0, y: 200.0 };
        let shifted_origin = Point { x: 300.0, y: 400.0 };
        let shifted_frame = Rect {
            x: 310.0,
            y: 420.0,
            ..frame
        };

        let cases = [
            (
                "titled element ignores frame",
                Some("OK"),
                info(Some("OK"), moved),
                origin,
                true,
            ),
            (
                "title change is stale",
                Some("OK"),
                info(Some("Cancel"), frame),
                origin,
                false,
            ),
            (
                "anonymous same frame",
                None,
                info(None, frame),
                origin,
                true,
            ),
            (
                "anonymous moved is stale",
                None,
                info(None, moved),
                origin,
                false,
            ),
            (
                "anonymous follows window",
                None,
                info(None, shifted_frame),
                shifted_origin,
                true,
            ),
        ];
        for (name, title, current, current_origin, want) in cases {
            let fp = Fingerprint::of(&info(title, frame), Some(origin));
            assert_eq!(fp.matches(&current, Some(current_origin)), want, "{name}");
        }
    }
}
