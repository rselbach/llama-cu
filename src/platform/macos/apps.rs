//! App discovery and launching.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2_app_kit::{
    NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration,
};
use objc2_foundation::{NSBundle, NSError, NSString, NSURL};

use super::ax;
use crate::error::{Error, ErrorCode, Result};
use crate::model::AppInfo;

/// How long to wait for Launch Services to start an app.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for a launched app to answer accessibility requests.
const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for a ready app to open its first window.
const WINDOW_TIMEOUT: Duration = Duration::from_secs(3);

/// Lists running regular apps and apps installed in the standard folders.
pub fn list() -> Vec<AppInfo> {
    let mut apps: Vec<AppInfo> = NSWorkspace::sharedWorkspace()
        .runningApplications()
        .iter()
        .filter(|app| {
            app.activationPolicy() == NSApplicationActivationPolicy::Regular && !app.isTerminated()
        })
        .map(|app| running_info(&app))
        .collect();

    for path in installed_paths() {
        let Ok(info) = info_at(&path) else {
            continue;
        };
        let duplicate = apps.iter().any(|a| match (&a.bundle_id, &info.bundle_id) {
            (Some(x), Some(y)) => x == y,
            _ => a.path == info.path,
        });
        if !duplicate {
            apps.push(info);
        }
    }
    apps
}

/// Describes the app bundle at `path`.
pub fn info_at(path: &Path) -> Result<AppInfo> {
    let not_found = || {
        Error::new(
            ErrorCode::AppNotFound,
            format!("{} is not an app bundle", path.display()),
        )
    };
    let path = path.canonicalize().map_err(|_| not_found())?;
    let path_str = path.to_str().ok_or_else(not_found)?;
    let bundle = NSBundle::bundleWithPath(&NSString::from_str(path_str)).ok_or_else(not_found)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(not_found)?
        .to_string();
    Ok(AppInfo {
        name,
        bundle_id: bundle.bundleIdentifier().map(|s| s.to_string()),
        path: Some(path_str.to_string()),
        pid: None,
    })
}

/// Reports whether `pid` is a running app.
pub fn is_running(pid: i32) -> bool {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .is_some_and(|app| !app.isTerminated())
}

/// Returns the process ID of the frontmost app.
pub fn frontmost_pid() -> Option<i32> {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|app| app.processIdentifier())
}

/// Launches an app and waits until it accepts accessibility requests.
pub fn launch(app: &AppInfo, background: bool) -> Result<i32> {
    let workspace = NSWorkspace::sharedWorkspace();
    let url = match (&app.path, &app.bundle_id) {
        (Some(path), _) => NSURL::fileURLWithPath(&NSString::from_str(path)),
        (None, Some(id)) => workspace
            .URLForApplicationWithBundleIdentifier(&NSString::from_str(id))
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::AppNotFound,
                    format!("no app with bundle ID {id}"),
                )
            })?,
        (None, None) => {
            return Err(Error::new(
                ErrorCode::AppNotFound,
                format!("{} has no path or bundle ID", app.name),
            ));
        }
    };

    let (tx, rx) = mpsc::channel::<std::result::Result<i32, String>>();
    let handler = RcBlock::new(
        move |running: *mut NSRunningApplication, error: *mut NSError| {
            let result = match (unsafe { running.as_ref() }, unsafe { error.as_ref() }) {
                (Some(running), _) => Ok(running.processIdentifier()),
                (None, Some(error)) => Err(error.localizedDescription().to_string()),
                (None, None) => Err("unknown error".to_string()),
            };
            let _ = tx.send(result);
        },
    );
    let config = NSWorkspaceOpenConfiguration::configuration();
    config.setActivates(!background);
    workspace.openApplicationAtURL_configuration_completionHandler(&url, &config, Some(&handler));

    let pid = match rx.recv_timeout(LAUNCH_TIMEOUT) {
        Ok(Ok(pid)) => pid,
        Ok(Err(reason)) => {
            return Err(Error::new(
                ErrorCode::Platform,
                format!("launching {}: {reason}", app.name),
            ));
        }
        Err(_) => {
            return Err(Error::new(
                ErrorCode::Timeout,
                format!("launching {} timed out", app.name),
            ));
        }
    };
    wait_until_ready(pid);
    Ok(pid)
}

/// Waits for the app to answer accessibility requests and open a window.
/// Gives up quietly: some apps never open windows, and missing permissions
/// are reported by the next command.
fn wait_until_ready(pid: i32) {
    let app = ax::application(pid);
    let start = Instant::now();
    let mut ready_at = None;
    while start.elapsed() < READY_TIMEOUT {
        match ax::elements_attribute(&app, "AXWindows") {
            Ok(windows) if !windows.is_empty() => return,
            Ok(_) => {
                let ready = *ready_at.get_or_insert_with(Instant::now);
                if ready.elapsed() >= WINDOW_TIMEOUT {
                    return;
                }
            }
            Err(e) if e.code == ErrorCode::PermissionDenied => return,
            Err(_) => {}
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn running_info(app: &NSRunningApplication) -> AppInfo {
    let path = app
        .bundleURL()
        .and_then(|u| u.path())
        .map(|p| p.to_string());
    let name = app
        .localizedName()
        .map(|n| n.to_string())
        .or_else(|| {
            path.as_deref()
                .and_then(|p| Path::new(p).file_stem())
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| format!("pid {}", app.processIdentifier()));
    AppInfo {
        name,
        bundle_id: app.bundleIdentifier().map(|s| s.to_string()),
        path,
        pid: Some(app.processIdentifier()),
    }
}

/// Returns app bundles in the standard folders and one level of subfolders,
/// such as Utilities.
fn installed_paths() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(Path::new(&home).join("Applications"));
    }
    let mut out = Vec::new();
    for root in roots {
        for entry in read_dir(&root) {
            if is_app(&entry) {
                out.push(entry);
            } else if entry.is_dir() {
                out.extend(read_dir(&entry).into_iter().filter(|p| is_app(p)));
            }
        }
    }
    out
}

fn read_dir(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default()
}

fn is_app(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "app")
}
