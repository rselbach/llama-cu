//! The agent cursor: a small window that shows where background input goes
//! without moving the real pointer. Each command exits after it acts, so a
//! helper process owns the window. Commands start the helper when needed and
//! ask it to move over a socket; it quits once the cursor has been idle.

use std::env;
use std::f64::consts::FRAC_PI_2;
use std::fs::{self, File, TryLockError};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEventMask,
    NSImage, NSImageView, NSNormalWindowLevel, NSPanel, NSScreen, NSWindowAnimationBehavior,
    NSWindowCollectionBehavior, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColor, CGColorSpace, CGContext,
    CGImageAlphaInfo, CGLineJoin,
};
use objc2_foundation::{
    NSActivityOptions, NSDate, NSDefaultRunLoopMode, NSPoint, NSProcessInfo, NSRect, NSSize,
    NSString,
};
use serde::{Deserialize, Serialize};

use super::background;
use super::input::DRAG_DURATION;
use crate::error::{Error, ErrorCode, Result};
use crate::model::Point;
use crate::platform::Gesture;
use crate::session::state_dir;

/// Longest wait for the cursor to arrive before input goes ahead anyway.
const ARRIVAL_TIMEOUT: Duration = Duration::from_secs(1);
/// Longest wait for a new helper to start listening.
const START_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest wait for a connected command to send its request.
const REQUEST_TIMEOUT: Duration = Duration::from_millis(200);
/// Shortest and longest moves. Longer distances take longer, up to the cap
/// that bounds how long a command waits.
const MOVE_MIN: Duration = Duration::from_millis(120);
const MOVE_MAX: Duration = Duration::from_millis(250);
/// The cursor fades out after this long without input.
const HIDE_AFTER: Duration = Duration::from_secs(30);
const FADE: Duration = Duration::from_millis(300);
/// The helper exits after this long without input.
const EXIT_AFTER: Duration = Duration::from_secs(300);
/// Frame interval while animating, and how often an idle helper looks for
/// requests.
const FRAME: Duration = Duration::from_millis(16);
const IDLE_POLL: Duration = Duration::from_millis(30);
/// How often the cursor restacks above its window and the helper checks its
/// socket.
const UPKEEP: Duration = Duration::from_millis(500);
/// Each click ring's duration, and the delay between rings of a double click.
const RIPPLE: Duration = Duration::from_millis(350);
const RIPPLE_GAP: Duration = Duration::from_millis(120);
const SCROLL_CUE: Duration = Duration::from_millis(400);

/// Side of the square cursor window in points, with the tip at its center.
const SIZE: f64 = 64.0;
/// Pixels per point of the cursor image.
const SCALE: f64 = 2.0;
/// The llama-cu icon's blue, and orange for right and middle clicks.
const BLUE: (f64, f64, f64) = (0.0, 0.33, 0.91);
const ORANGE: (f64, f64, f64) = (1.0, 0.55, 0.0);
const ARRIVED: &str = "arrived";

/// A command's request to the helper.
#[derive(Serialize, Deserialize)]
struct Request {
    window: u64,
    at: Point,
    gesture: Gesture,
}

/// Asks the helper to show the cursor, starting it when none is running,
/// and waits until the cursor arrives.
pub fn show(window: u64, at: Point, gesture: Gesture) -> Result<()> {
    let stream = connect()?;
    stream.set_read_timeout(Some(ARRIVAL_TIMEOUT))?;
    let mut request = serde_json::to_vec(&Request {
        window,
        at,
        gesture,
    })?;
    request.push(b'\n');
    (&stream).write_all(&request)?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    if reply.trim_end() != ARRIVED {
        return Err(Error::new(
            ErrorCode::Platform,
            "the cursor helper did not answer",
        ));
    }
    Ok(())
}

fn connect() -> Result<UnixStream> {
    let path = socket_path();
    if let Ok(stream) = UnixStream::connect(&path) {
        return Ok(stream);
    }
    start_helper()?;
    let started = Instant::now();
    loop {
        match UnixStream::connect(&path) {
            Ok(stream) => return Ok(stream),
            Err(err) if started.elapsed() >= START_TIMEOUT => return Err(err.into()),
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// Starts the helper from this executable in its own process group, so it
/// outlives the command. It inherits no output an agent might wait on.
fn start_helper() -> Result<()> {
    Command::new(env::current_exe()?)
        .arg("cursor-helper")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    Ok(())
}

/// The helper's socket. The version keeps a newer llama-cu from talking to
/// an older helper.
fn socket_path() -> PathBuf {
    state_dir().join(format!("cursor-{}.sock", env!("CARGO_PKG_VERSION")))
}

/// Runs the helper until it has been idle for a while or its socket is
/// removed. Returns at once when another helper already runs.
pub fn run() -> Result<()> {
    let mtm = MainThreadMarker::new().ok_or_else(|| {
        Error::new(
            ErrorCode::Platform,
            "the cursor helper must run on the main thread",
        )
    })?;
    let path = socket_path();
    fs::create_dir_all(state_dir())?;
    let lock = File::create(path.with_extension("lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(err)) => return Err(err.into()),
    }
    match fs::remove_file(&path) {
        Err(err) if err.kind() != ErrorKind::NotFound => return Err(err.into()),
        _ => {}
    }
    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;
    let socket = fs::metadata(&path)?.ino();

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();
    // Keep App Nap from slowing the animation, without keeping the Mac awake.
    let _activity = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
        NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
        &NSString::from_str("Showing the llama-cu agent cursor"),
    );
    let mut cursor = Cursor::new(mtm);
    let mut upkept = Instant::now();
    loop {
        while let Some((request, stream)) = accept(&listener) {
            cursor.start(request, stream);
        }
        let now = Instant::now();
        cursor.tick(now);
        if now.duration_since(upkept) >= UPKEEP {
            upkept = now;
            if cursor.idle_for(now) >= EXIT_AFTER || !is_ours(&path, socket) {
                break;
            }
            cursor.upkeep(now);
        }
        let wait = if cursor.is_animating() {
            FRAME
        } else {
            IDLE_POLL
        };
        pump(&app, wait);
    }
    if is_ours(&path, socket) {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// Returns the next pending request. Connections that fail or send no
/// valid request are dropped; their commands go ahead without the cursor.
fn accept(listener: &UnixListener) -> Option<(Request, UnixStream)> {
    loop {
        let (stream, _) = listener.accept().ok()?;
        if let Some(request) = read_request(&stream) {
            return Some((request, stream));
        }
    }
}

fn read_request(stream: &UnixStream) -> Option<Request> {
    // Accepted sockets inherit the listener's nonblocking mode on macOS.
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT)).ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

/// Reports whether the socket file is still the one this helper bound.
fn is_ours(path: &Path, socket: u64) -> bool {
    fs::metadata(path).is_ok_and(|m| m.ino() == socket)
}

/// Handles window server events for up to `wait`, which also lets AppKit
/// redraw the cursor.
fn pump(app: &NSApplication, wait: Duration) {
    let until = NSDate::dateWithTimeIntervalSinceNow(wait.as_secs_f64());
    let mode = unsafe { NSDefaultRunLoopMode };
    if let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
        NSEventMask::Any,
        Some(&until),
        mode,
        true,
    ) {
        app.sendEvent(&event);
    }
}

/// A move of the cursor tip along a gentle curve.
struct Motion {
    from: Point,
    control: Point,
    to: Point,
    started: Instant,
    duration: Duration,
    /// What to show on arrival, or `None` while following a drag.
    then: Option<Gesture>,
}

impl Motion {
    fn new(
        from: Point,
        to: Point,
        started: Instant,
        duration: Duration,
        then: Option<Gesture>,
    ) -> Self {
        // Bow to one side of the straight line, as a hand moving a mouse does.
        let control = Point {
            x: (from.x + to.x) / 2.0 + (to.y - from.y) * 0.12,
            y: (from.y + to.y) / 2.0 - (to.x - from.x) * 0.12,
        };
        Self {
            from,
            control,
            to,
            started,
            duration,
            then,
        }
    }

    /// Returns the eased progress at `now`, from 0 to 1.
    fn progress(&self, now: Instant) -> f64 {
        ease(progress(self.started, self.duration, now))
    }

    /// Returns the point on the curve at progress `t`.
    fn at(&self, t: f64) -> Point {
        let u = 1.0 - t;
        Point {
            x: u * u * self.from.x + 2.0 * u * t * self.control.x + t * t * self.to.x,
            y: u * u * self.from.y + 2.0 * u * t * self.control.y + t * t * self.to.y,
        }
    }
}

/// An animation the cursor plays where it stands.
enum Effect {
    Ripples {
        started: Instant,
        count: u32,
        secondary: bool,
    },
    Scroll {
        started: Instant,
        dx: i32,
        dy: i32,
    },
}

/// One frame's appearance, compared to skip redrawing an unchanged image.
#[derive(Default, PartialEq)]
struct Look {
    pressed: bool,
    /// Radius and opacity of each click ring.
    rings: Vec<(f64, f64)>,
    secondary: bool,
    /// Scroll direction angle, how far the chevrons have travelled, and
    /// their opacity.
    chevrons: Option<(f64, f64, f64)>,
}

/// The cursor window and what it is doing.
struct Cursor {
    mtm: MainThreadMarker,
    panel: Retained<NSPanel>,
    view: Retained<NSImageView>,
    /// The window the cursor stays just above.
    window: u64,
    /// The tip in screen points, or `None` while hidden.
    tip: Option<Point>,
    motion: Option<Motion>,
    effect: Option<Effect>,
    /// The command waiting for the cursor to arrive.
    waiting: Option<UnixStream>,
    last_used: Instant,
    fading: Option<Instant>,
    drawn: Look,
}

impl Cursor {
    fn new(mtm: MainThreadMarker) -> Self {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(SIZE, SIZE));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setTitle(&NSString::from_str("llama-cu cursor"));
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setIgnoresMouseEvents(true);
        panel.setHidesOnDeactivate(false);
        panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
        // Ordering above another app's window works only within one level.
        panel.setLevel(NSNormalWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        let drawn = Look::default();
        let view = NSImageView::imageViewWithImage(&render(&drawn), mtm);
        panel.setContentView(Some(&view));
        Self {
            mtm,
            panel,
            view,
            window: 0,
            tip: None,
            motion: None,
            effect: None,
            waiting: None,
            last_used: Instant::now(),
            fading: None,
            drawn,
        }
    }

    fn start(&mut self, request: Request, stream: UnixStream) {
        // A newer request supersedes one still moving; its command goes on.
        self.reply();
        let now = Instant::now();
        self.last_used = now;
        self.waiting = Some(stream);
        if !background::is_on_screen(request.window) {
            self.hide();
            return;
        }
        self.window = request.window;
        self.fading = None;
        self.panel.setAlphaValue(1.0);
        self.effect = None;
        match self.tip {
            Some(from) if distance(from, request.at) >= 1.0 => {
                let duration = move_duration(from, request.at);
                self.motion = Some(Motion::new(
                    from,
                    request.at,
                    now,
                    duration,
                    Some(request.gesture),
                ));
            }
            // A hidden cursor appears where it is needed instead of flying in.
            _ => {
                self.motion = None;
                self.place(request.at);
                self.arrive(request.gesture, now);
            }
        }
        self.restack();
    }

    /// Answers the waiting command and starts what the gesture shows.
    fn arrive(&mut self, gesture: Gesture, now: Instant) {
        self.reply();
        self.effect = match gesture {
            Gesture::Point => None,
            Gesture::Click { count, secondary } => Some(Effect::Ripples {
                started: now,
                count: count.max(1),
                secondary,
            }),
            Gesture::Scroll { dx, dy } => Some(Effect::Scroll {
                started: now,
                dx,
                dy,
            }),
            Gesture::Drag { to } => {
                let from = self.tip.unwrap_or(to);
                self.motion = Some(Motion::new(from, to, now, DRAG_DURATION, None));
                None
            }
        };
    }

    fn tick(&mut self, now: Instant) {
        if let Some(motion) = &self.motion {
            let t = motion.progress(now);
            let tip = motion.at(t);
            let then = motion.then;
            self.place(tip);
            if t >= 1.0 {
                self.motion = None;
                if let Some(gesture) = then {
                    self.arrive(gesture, now);
                }
            }
        }
        if let Some(started) = self.fading {
            let t = progress(started, FADE, now);
            self.panel.setAlphaValue(1.0 - t);
            if t >= 1.0 {
                self.hide();
            }
        }
        let look = self.look(now);
        if look != self.drawn {
            self.view.setImage(Some(&render(&look)));
            self.drawn = look;
        }
    }

    /// Restacks the cursor above its window, hides it when that window is
    /// gone or off screen, and starts fading it once idle.
    fn upkeep(&mut self, now: Instant) {
        if self.tip.is_none() {
            return;
        }
        if !background::is_on_screen(self.window) {
            self.hide();
            return;
        }
        self.restack();
        if self.fading.is_none() && !self.is_animating() && self.idle_for(now) >= HIDE_AFTER {
            self.fading = Some(now);
        }
    }

    fn idle_for(&self, now: Instant) -> Duration {
        if self.is_animating() || self.waiting.is_some() {
            return Duration::ZERO;
        }
        now.duration_since(self.last_used)
    }

    fn is_animating(&self) -> bool {
        self.motion.is_some() || self.effect.is_some() || self.fading.is_some()
    }

    fn look(&mut self, now: Instant) -> Look {
        let pressed = self.motion.as_ref().is_some_and(|m| m.then.is_none());
        let mut look = Look {
            pressed,
            ..Look::default()
        };
        match self.effect {
            Some(Effect::Ripples {
                started,
                count,
                secondary,
            }) => {
                look.secondary = secondary;
                for ring in 0..count {
                    let start = started + RIPPLE_GAP * ring;
                    if now < start {
                        continue;
                    }
                    let t = progress(start, RIPPLE, now);
                    if t < 1.0 {
                        let grown = 1.0 - (1.0 - t) * (1.0 - t);
                        look.rings.push((4.0 + 18.0 * grown, 0.75 * (1.0 - t)));
                    }
                }
                let last = started + RIPPLE_GAP * (count - 1) + RIPPLE;
                if now >= last {
                    self.effect = None;
                }
            }
            Some(Effect::Scroll { started, dx, dy }) => {
                let t = progress(started, SCROLL_CUE, now);
                let angle = f64::from(dy.signum()).atan2(f64::from(dx.signum()));
                look.chevrons = Some((angle, 8.0 * t, 0.9 * (1.0 - t)));
                if t >= 1.0 {
                    self.effect = None;
                    look.chevrons = None;
                }
            }
            None => {}
        }
        look
    }

    fn place(&mut self, tip: Point) {
        self.tip = Some(tip);
        let screens = NSScreen::screens(self.mtm);
        let Some(primary) = screens.firstObject() else {
            return;
        };
        // AppKit measures up from the bottom of the primary display; screen
        // points measure down from its top.
        let top = primary.frame().size.height;
        let origin = NSPoint::new(tip.x - SIZE / 2.0, top - tip.y - SIZE / 2.0);
        self.panel.setFrameOrigin(origin);
    }

    fn restack(&self) {
        self.panel
            .orderWindow_relativeTo(NSWindowOrderingMode::Above, self.window as isize);
    }

    fn hide(&mut self) {
        self.reply();
        self.panel.orderOut(None);
        self.tip = None;
        self.motion = None;
        self.effect = None;
        self.fading = None;
    }

    fn reply(&mut self) {
        if let Some(stream) = self.waiting.take() {
            // The command may have stopped waiting; it went ahead either way.
            let _ = (&stream).write_all(format!("{ARRIVED}\n").as_bytes());
        }
    }
}

/// Returns linear progress from `started` over `duration`, from 0 to 1.
fn progress(started: Instant, duration: Duration, now: Instant) -> f64 {
    let elapsed = now.saturating_duration_since(started).as_secs_f64();
    (elapsed / duration.as_secs_f64()).min(1.0)
}

/// Eases in and out, so moves start and stop gently.
fn ease(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

fn distance(a: Point, b: Point) -> f64 {
    (b.x - a.x).hypot(b.y - a.y)
}

/// Returns how long a move takes: longer for longer distances, within
/// bounds.
fn move_duration(from: Point, to: Point) -> Duration {
    let millis = MOVE_MIN.as_secs_f64() * 1000.0 + distance(from, to) * 0.25;
    Duration::from_secs_f64(millis / 1000.0).clamp(MOVE_MIN, MOVE_MAX)
}

/// Draws one frame of the cursor: click rings or scroll chevrons, then the
/// arrow, with the tip at the center.
fn render(look: &Look) -> Retained<NSImage> {
    let pixels = (SIZE * SCALE) as usize;
    let space = CGColorSpace::new_device_rgb();
    let context = unsafe {
        CGBitmapContextCreate(
            ptr::null_mut(),
            pixels,
            pixels,
            8,
            0,
            space.as_deref(),
            CGImageAlphaInfo::PremultipliedLast.0,
        )
    };
    let size = NSSize::new(SIZE, SIZE);
    let image = context.as_deref().and_then(|c| {
        draw(c, look);
        CGBitmapContextCreateImage(Some(c))
    });
    match image {
        Some(image) => NSImage::initWithCGImage_size(NSImage::alloc(), &image, size),
        None => NSImage::initWithSize(NSImage::alloc(), size),
    }
}

fn draw(c: &CGContext, look: &Look) {
    let c = Some(c);
    // Draw in points with y growing downward and the tip at the origin.
    CGContext::translate_ctm(c, 0.0, SIZE * SCALE);
    CGContext::scale_ctm(c, SCALE, -SCALE);
    CGContext::translate_ctm(c, SIZE / 2.0, SIZE / 2.0);
    CGContext::set_line_join(c, CGLineJoin::Round);

    // A white halo under each stroke keeps it visible on dark content.
    let (r, g, b) = if look.secondary { ORANGE } else { BLUE };
    for &(radius, alpha) in &look.rings {
        CGContext::set_line_width(c, 4.5);
        CGContext::set_rgb_stroke_color(c, 1.0, 1.0, 1.0, alpha * 0.6);
        CGContext::stroke_ellipse_in_rect(c, circle(radius));
        CGContext::set_line_width(c, 2.5);
        CGContext::set_rgb_stroke_color(c, r, g, b, alpha);
        CGContext::stroke_ellipse_in_rect(c, circle(radius));
    }
    if let Some((angle, travel, alpha)) = look.chevrons {
        CGContext::save_g_state(c);
        // Beside the tip, clear of the arrow: left of it for vertical
        // scrolls and above it for horizontal ones.
        if angle.sin().abs() > 0.5 {
            CGContext::translate_ctm(c, -13.0, -4.0);
        } else {
            CGContext::translate_ctm(c, -4.0, -13.0);
        }
        // Chevrons are drawn pointing down, then turned toward the scroll.
        CGContext::rotate_ctm(c, angle - FRAC_PI_2);
        for (width, (r, g, b), opacity) in [(4.5, (1.0, 1.0, 1.0), 0.6), (2.5, BLUE, 1.0)] {
            CGContext::set_line_width(c, width);
            CGContext::set_rgb_stroke_color(c, r, g, b, alpha * opacity);
            for step in [0.0, 6.0] {
                let y = travel + step;
                CGContext::begin_path(c);
                CGContext::move_to_point(c, -4.5, y - 3.5);
                CGContext::add_line_to_point(c, 0.0, y + 1.0);
                CGContext::add_line_to_point(c, 4.5, y - 3.5);
                CGContext::stroke_path(c);
            }
        }
        CGContext::restore_g_state(c);
    }

    if look.pressed {
        CGContext::scale_ctm(c, 0.85, 0.85);
    }
    CGContext::save_g_state(c);
    let shadow = CGColor::new_srgb(0.0, 0.0, 0.0, 0.35);
    CGContext::set_shadow_with_color(c, CGSize::new(0.0, -3.0), 6.0, Some(&shadow));
    arrow(c);
    CGContext::set_rgb_fill_color(c, BLUE.0, BLUE.1, BLUE.2, 1.0);
    CGContext::fill_path(c);
    CGContext::restore_g_state(c);
    arrow(c);
    CGContext::set_line_width(c, 1.75);
    CGContext::set_rgb_stroke_color(c, 1.0, 1.0, 1.0, 1.0);
    CGContext::stroke_path(c);
}

/// Adds the arrow outline, pointing up and to the left from the tip.
fn arrow(c: Option<&CGContext>) {
    CGContext::begin_path(c);
    CGContext::move_to_point(c, 0.0, 0.0);
    CGContext::add_line_to_point(c, 0.0, 18.5);
    CGContext::add_line_to_point(c, 4.9, 14.1);
    CGContext::add_line_to_point(c, 12.9, 13.4);
    CGContext::close_path(c);
}

fn circle(radius: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(-radius, -radius),
        CGSize::new(radius * 2.0, radius * 2.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_take_longer_with_distance_within_bounds() {
        let origin = Point { x: 0.0, y: 0.0 };
        let cases = [
            ("no distance", 0.0, MOVE_MIN),
            ("medium move", 400.0, Duration::from_millis(220)),
            ("long move", 5000.0, MOVE_MAX),
        ];
        for (name, x, want) in cases {
            let got = move_duration(origin, Point { x, y: 0.0 });
            let diff = got.abs_diff(want);
            assert!(
                diff < Duration::from_millis(1),
                "{name}: got {got:?}, want {want:?}"
            );
        }
    }

    #[test]
    fn motion_starts_and_ends_at_its_points() {
        let from = Point { x: 10.0, y: 20.0 };
        let to = Point { x: 300.0, y: -40.0 };
        let motion = Motion::new(from, to, Instant::now(), MOVE_MAX, None);
        assert_eq!(motion.at(0.0), from);
        assert_eq!(motion.at(1.0), to);
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
    }
}
