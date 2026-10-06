//! llama-cu: let agents operate desktop apps through accessibility APIs,
//! window screenshots, and synthetic input.

mod commands;
mod error;
mod keys;
mod model;
mod platform;
mod render;
mod session;
mod text;

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use serde::Serialize;

use crate::commands::{Ctx, DEFAULT_MAX_NODES, Direction, PasteFormat, SelectPosition, Target};
use crate::error::{Error, ErrorCode, Result};
use crate::model::{MouseButton, Point};
use crate::render::{Render, TextLimit};
use crate::session::Store;

#[derive(Parser)]
#[command(name = "llama-cu", version, about = "Operate desktop apps for agents.")]
struct Cli {
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,

    /// Experimental input without activating the app or moving the real pointer.
    #[arg(
        long,
        global = true,
        env = "LLAMA_CU_BACKGROUND",
        value_parser = clap::builder::FalseyValueParser::new()
    )]
    background: bool,

    /// Session name. Each session keeps its own selected app and element IDs.
    #[arg(
        long,
        global = true,
        env = "LLAMA_CU_SESSION",
        default_value = "default"
    )]
    session: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List installed apps and whether they are running.
    ListApps {
        /// Show only running apps.
        #[arg(long)]
        running: bool,
    },
    /// Select an app by name, path, or bundle ID; launch it if needed.
    GetApp {
        /// App name, .app path, or bundle ID.
        app: String,
    },
    /// Read the selected app's accessibility tree.
    GetAxState(StateArgs),
    /// Capture the selected app's window as PNG.
    GetScreenshot(ScreenshotArgs),
    /// Read the accessibility tree and capture the same window.
    GetAxStateAndScreenshot {
        #[command(flatten)]
        state: StateArgs,
        /// Write the PNG here instead of a temporary file.
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Click an element or window-relative coordinates.
    Click {
        #[command(flatten)]
        target: RequiredTarget,
        /// Mouse button.
        #[arg(long, value_enum, default_value_t)]
        button: MouseButton,
        /// Number of clicks, such as 2 for a double click.
        #[arg(long, default_value_t = 1)]
        count: u32,
        #[command(flatten)]
        observe: Observe,
    },
    /// Drag with the left button between window-relative coordinates.
    Drag {
        /// Start point as X,Y.
        #[arg(long)]
        from: Point,
        /// End point as X,Y.
        #[arg(long)]
        to: Point,
        #[command(flatten)]
        observe: Observe,
    },
    /// Scroll at an element, window-relative coordinates, or the window center.
    Scroll {
        #[command(flatten)]
        target: OptionalTarget,
        /// Scroll direction.
        #[arg(long, value_enum)]
        direction: Direction,
        /// Number of lines to scroll.
        #[arg(long, default_value_t = 5)]
        amount: u32,
        #[command(flatten)]
        observe: Observe,
    },
    /// Press keys and shortcuts in order, such as `cmd+shift+t` or `down enter`.
    PressKey {
        /// Keys or combos: modifiers cmd, ctrl, alt/option, shift, fn joined with +.
        #[arg(required = true)]
        keys: Vec<String>,
        #[command(flatten)]
        observe: Observe,
    },
    /// Type text into the focused control.
    TypeText {
        /// Text to type; `-` reads standard input.
        text: String,
        #[command(flatten)]
        observe: Observe,
    },
    /// Paste text through the clipboard, then restore the previous clipboard.
    Paste {
        /// Content format. Markdown and HTML paste as rich text with a plain-text fallback.
        #[arg(long, value_enum, default_value_t)]
        format: PasteFormat,
        /// Content to paste; `-` reads standard input.
        text: String,
        #[command(flatten)]
        observe: Observe,
    },
    /// Replace an editable control's value directly.
    SetValue {
        /// Element ID; defaults to the focused element.
        #[arg(long)]
        element: Option<usize>,
        /// New value; `-` reads standard input.
        value: String,
        #[command(flatten)]
        observe: Observe,
    },
    /// Select matching text, or put the cursor before or after it.
    SelectText {
        /// Element ID; defaults to the focused element.
        #[arg(long)]
        element: Option<usize>,
        /// Text to find.
        text: String,
        /// Select the match or place the cursor before or after it.
        #[arg(long, value_enum, default_value_t)]
        position: SelectPosition,
        /// Which match to use, starting at 1.
        #[arg(long, default_value_t = 1)]
        occurrence: usize,
        #[command(flatten)]
        observe: Observe,
    },
    /// Invoke an accessibility action, such as showMenu, increment, or a custom action.
    PerformSecondaryAction {
        /// Element ID.
        #[arg(long)]
        element: usize,
        /// Action name from get-ax-state output.
        action: String,
        #[command(flatten)]
        observe: Observe,
    },
    /// Check, and optionally request, the permissions llama-cu needs.
    Doctor {
        /// Open the system permission prompts for anything missing.
        #[arg(long)]
        prompt: bool,
    },
}

#[derive(Args)]
struct StateArgs {
    /// Window ID from get-app; defaults to the focused window.
    #[arg(long)]
    window: Option<u64>,
    /// Maximum number of elements to list.
    #[arg(long, default_value_t = DEFAULT_MAX_NODES)]
    max_nodes: usize,
    /// Most characters of each text shown, or `max` for full text.
    #[arg(long, default_value_t)]
    text_limit: TextLimit,
}

#[derive(Args)]
struct Observe {
    /// After the action, print the app's state and a screenshot, as
    /// get-ax-state-and-screenshot does.
    #[arg(long)]
    observe: bool,
}

#[derive(Args)]
struct ScreenshotArgs {
    /// Window ID from get-app; defaults to the focused window.
    #[arg(long)]
    window: Option<u64>,
    /// Write the PNG here instead of a temporary file.
    #[arg(long, short)]
    output: Option<PathBuf>,
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct RequiredTarget {
    /// Element ID from get-ax-state.
    #[arg(long)]
    element: Option<usize>,
    /// Window-relative point as X,Y, matching screenshot pixels.
    #[arg(long)]
    at: Option<Point>,
}

#[derive(Args)]
#[group(multiple = false)]
struct OptionalTarget {
    /// Element ID from get-ax-state.
    #[arg(long)]
    element: Option<usize>,
    /// Window-relative point as X,Y, matching screenshot pixels.
    #[arg(long)]
    at: Option<Point>,
}

impl OptionalTarget {
    fn target(&self) -> Option<Target> {
        match (self.element, self.at) {
            (Some(id), _) => Some(Target::Element(id)),
            (None, Some(p)) => Some(Target::Point(p)),
            (None, None) => None,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if cli.json {
                let body = serde_json::json!({ "error": err });
                eprintln!("{body}");
            } else {
                eprintln!("error: {err}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    platform::prepare_process()?;
    let store = Store::open(&cli.session)?;
    let mut ctx = Ctx::new(platform::current(), store, cli.background)?;
    let json = cli.json;
    let (mut action, observe) = match &cli.command {
        Command::ListApps { running } => return emit(json, &ctx.list_apps(*running)?),
        Command::GetApp { app } => return emit(json, &ctx.get_app(app)?),
        Command::GetAxState(args) => {
            let state = ctx.get_ax_state(args.window, args.max_nodes, args.text_limit)?;
            return emit(json, &state);
        }
        Command::GetScreenshot(args) => {
            return emit(json, &ctx.get_screenshot(args.window, args.output.clone())?);
        }
        Command::GetAxStateAndScreenshot { state, output } => {
            let result = ctx.get_ax_state_and_screenshot(
                state.window,
                state.max_nodes,
                state.text_limit,
                output.clone(),
            )?;
            return emit(json, &result);
        }
        Command::Doctor { prompt } => return emit(json, &ctx.doctor(*prompt)),
        Command::Click {
            target,
            button,
            count,
            observe,
        } => {
            let target = match (target.element, target.at) {
                (Some(id), _) => Target::Element(id),
                (None, Some(p)) => Target::Point(p),
                (None, None) => unreachable!("clap requires --element or --at"),
            };
            (ctx.click(target, *button, *count)?, observe)
        }
        Command::Drag { from, to, observe } => (ctx.drag(*from, *to)?, observe),
        Command::Scroll {
            target,
            direction,
            amount,
            observe,
        } => (ctx.scroll(target.target(), *direction, *amount)?, observe),
        Command::PressKey { keys, observe } => (ctx.press_key(keys)?, observe),
        Command::TypeText { text, observe } => (ctx.type_text(&read_arg(text)?)?, observe),
        Command::Paste {
            format,
            text,
            observe,
        } => (ctx.paste(&read_arg(text)?, *format)?, observe),
        Command::SetValue {
            element,
            value,
            observe,
        } => (ctx.set_value(*element, &read_arg(value)?)?, observe),
        Command::SelectText {
            element,
            text,
            position,
            occurrence,
            observe,
        } => (
            ctx.select_text(*element, text, *position, *occurrence)?,
            observe,
        ),
        Command::PerformSecondaryAction {
            element,
            action,
            observe,
        } => (ctx.perform_secondary_action(*element, action)?, observe),
    };
    if observe.observe {
        action.observed = Some(ctx.observe());
    }
    emit(json, &action)
}

fn emit<T: Serialize + Render>(json: bool, value: &T) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(value)?);
    } else {
        println!("{}", value.render());
    }
    Ok(())
}

/// Returns `arg`, or standard input when `arg` is `-`.
fn read_arg(arg: &str) -> Result<String> {
    if arg != "-" {
        return Ok(arg.to_string());
    }
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| Error::new(ErrorCode::Io, format!("reading standard input: {e}")))?;
    Ok(buf)
}
