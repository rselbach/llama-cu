//! Window capture with ScreenCaptureKit.

use std::path::Path;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_core_foundation::{CFRetained, CFString, CFURL, CFURLPathStyle};
use objc2_core_graphics::{CGImage, CGMainDisplayID, CGPreflightScreenCaptureAccess};
use objc2_foundation::NSError;
use objc2_image_io::CGImageDestination;
use objc2_screen_capture_kit::{
    SCCaptureResolutionType, SCContentFilter, SCScreenshotManager, SCShareableContent,
    SCStreamConfiguration,
};

use crate::error::{Error, ErrorCode, Result};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

/// Moves a value across the thread boundary of a completion handler.
struct Handoff<T>(T);

// SAFETY: the wrapped Objective-C and CoreFoundation objects are immutable
// results handed from the completion queue to the waiting thread.
unsafe impl<T> Send for Handoff<T> {}

/// Captures a window at `scale` pixels per point and writes it as PNG.
pub fn capture_window(window_id: u64, path: &Path, scale: f64) -> Result<(u32, u32)> {
    if !CGPreflightScreenCaptureAccess() {
        return Err(Error::new(
            ErrorCode::PermissionDenied,
            "screen recording permission is missing; run `llama-cu doctor`",
        ));
    }
    // Connects this command-line process to the window server.
    let _ = CGMainDisplayID();

    let content = shareable_content()?;
    let windows = unsafe { content.windows() };
    let window = windows
        .iter()
        .find(|w| u64::from(unsafe { w.windowID() }) == window_id)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::WindowNotFound,
                format!(
                    "window {window_id} is not on screen; it may be minimized or on another Space"
                ),
            )
        })?;

    let filter = unsafe {
        SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window)
    };
    let rect = unsafe { filter.contentRect() };
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth((rect.size.width * scale).round().max(1.0) as usize);
        config.setHeight((rect.size.height * scale).round().max(1.0) as usize);
        config.setCaptureResolution(SCCaptureResolutionType::Nominal);
        config.setScalesToFit(true);
        config.setShowsCursor(false);
        config.setIgnoreShadowsSingleWindow(true);
    }

    let image = screenshot(&filter, &config)?;
    write_png(&image, path)?;
    Ok((
        CGImage::width(Some(&image)) as u32,
        CGImage::height(Some(&image)) as u32,
    ))
}

fn shareable_content() -> Result<Retained<SCShareableContent>> {
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = match unsafe { Retained::retain(content) } {
                Some(content) => Ok(Handoff(content)),
                None => Err(describe(error)),
            };
            let _ = tx.send(result);
        },
    );
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, true, &handler,
        );
    }
    match rx.recv_timeout(CAPTURE_TIMEOUT) {
        Ok(Ok(Handoff(content))) => Ok(content),
        Ok(Err(reason)) => Err(capture_error(reason)),
        Err(_) => Err(Error::new(ErrorCode::Timeout, "listing windows timed out")),
    }
}

fn screenshot(
    filter: &SCContentFilter,
    config: &SCStreamConfiguration,
) -> Result<CFRetained<CGImage>> {
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        let result = match NonNull::new(image) {
            Some(image) => Ok(Handoff(unsafe { CFRetained::retain(image) })),
            None => Err(describe(error)),
        };
        let _ = tx.send(result);
    });
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            filter,
            config,
            Some(&handler),
        );
    }
    match rx.recv_timeout(CAPTURE_TIMEOUT) {
        Ok(Ok(Handoff(image))) => Ok(image),
        Ok(Err(reason)) => Err(capture_error(reason)),
        Err(_) => Err(Error::new(
            ErrorCode::Timeout,
            "capturing the window timed out",
        )),
    }
}

fn write_png(image: &CGImage, path: &Path) -> Result<()> {
    let failed = || {
        Error::new(
            ErrorCode::Io,
            format!("writing PNG to {} failed", path.display()),
        )
    };
    let path_str = path.to_str().ok_or_else(failed)?;
    let url = CFURL::with_file_system_path(
        None,
        Some(&CFString::from_str(path_str)),
        CFURLPathStyle::CFURLPOSIXPathStyle,
        false,
    )
    .ok_or_else(failed)?;
    let destination = unsafe {
        CGImageDestination::with_url(&url, &CFString::from_static_str("public.png"), 1, None)
    }
    .ok_or_else(failed)?;
    unsafe {
        destination.add_image(image, None);
        if !destination.finalize() {
            return Err(failed());
        }
    }
    Ok(())
}

fn describe(error: *mut NSError) -> String {
    unsafe { error.as_ref() }
        .map(|e| e.localizedDescription().to_string())
        .unwrap_or_else(|| "unknown error".to_string())
}

fn capture_error(reason: String) -> Error {
    Error::new(
        ErrorCode::Platform,
        format!("screen capture failed: {reason}"),
    )
}
