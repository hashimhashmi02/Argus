//! `argus-cli`: drive the engine by hand.
//!
//! This exists so the engine can be exercised and debugged before any UI does:
//! if the system is not usable from here, it is not ready for a UI to be built
//! on top of it. It also does what building a client always does to a protocol
//! -- the first gap it found is why `SessionInfo` now carries `status_for_ms`.
//!
//! Every command is one request over the named pipe and one reply.
//!
//! # Exit codes
//!
//! Distinct, so a script can tell "the engine said no" from "there is no
//! engine":
//!
//! | code | meaning |
//! |---|---|
//! | 0 | success |
//! | 1 | the engine refused the request, or the call failed |
//! | 2 | bad command line (from `clap`) |
//! | 3 | the engine is not running |

use std::process::ExitCode;

use anyhow::{Context, Result};
use argus_control_server::{Client, ClientError, default_pipe_name, pipe_path};
use argus_protocol::{Reply, Request};
use clap::{Parser, Subcommand};

mod render;

#[derive(Parser)]
#[command(
    name = "argus-cli",
    version,
    about = "Drive the Argus engine from the command line"
)]
struct Cli {
    /// Named pipe the engine listens on: a bare name like `argus-test`, or a
    /// full `\\.\pipe\...` path. Defaults to the current user's.
    #[arg(long, global = true, value_name = "NAME")]
    pipe: Option<String>,

    /// Print the engine's raw JSON reply instead of formatted text.
    ///
    /// For scripts. Fields and order are exactly what the engine sent.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List every session, most urgent first.
    #[command(alias = "ls")]
    List,

    /// Launch an agent from its manifest.
    Spawn {
        /// Manifest id, e.g. `shell` or `claude-code`.
        agent: String,

        /// Working directory for the agent.
        #[arg(long, value_name = "DIR")]
        cwd: Option<String>,

        /// Terminal size, as COLSxROWS.
        #[arg(long, value_name = "COLSxROWS", value_parser = parse_size)]
        size: Option<(u16, u16)>,
    },

    /// Show a session's screen, exactly as the user would see it.
    Screen {
        /// Session id, or any prefix naming exactly one session.
        id: String,
    },

    /// Type into a session.
    ///
    /// The words are joined with spaces and followed by Enter, so
    /// `send 3f echo hello` runs `echo hello`.
    Send {
        /// Session id, or any prefix naming exactly one session.
        id: String,

        /// The text to type.
        #[arg(required_unless_present = "key", trailing_var_arg = true)]
        text: Vec<String>,

        /// Send a named key instead of text: enter, escape, tab, up, down,
        /// ctrl-c, and the rest of the names a manifest uses.
        #[arg(long, conflicts_with = "text", value_name = "KEY")]
        key: Option<String>,

        /// Do not press Enter after the text.
        #[arg(long, conflicts_with = "key")]
        no_enter: bool,
    },

    /// Change a session's terminal size.
    Resize { id: String, cols: u16, rows: u16 },

    /// Stop a session. Killing one that has already finished succeeds.
    Kill { id: String },
}

/// `120x30` -> `(120, 30)`.
fn parse_size(s: &str) -> Result<(u16, u16), String> {
    let (cols, rows) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("`{s}` is not COLSxROWS, e.g. 120x30"))?;
    let cols: u16 = cols
        .parse()
        .map_err(|_| format!("bad column count `{cols}`"))?;
    let rows: u16 = rows
        .parse()
        .map_err(|_| format!("bad row count `{rows}`"))?;
    if cols == 0 || rows == 0 {
        return Err("a terminal needs at least one column and one row".into());
    }
    Ok((cols, rows))
}

/// How long to wait between typing text and pressing Enter.
///
/// Enter has to arrive as its own keystroke. Found by driving a real Claude
/// Code session: text and `\r` written in one burst are read by the TUI as a
/// *paste*, and inside a paste Enter is just a newline in the input box -- so the
/// prompt was typed and never submitted, and sat there for a full minute. A
/// person never hits this because they press Enter separately; a program has to
/// do the same. Most modern TUIs detect pastes this way, so this is not a
/// Claude-specific workaround.
///
/// The gap only has to be long enough for the two writes to reach the child as
/// separate reads. 100ms is far more than that needs and too short for a person
/// to notice.
const ENTER_GAP: std::time::Duration = std::time::Duration::from_millis(100);

impl Command {
    /// The requests this command sends, in order.
    ///
    /// Usually one. `send` with text and Enter is two -- see [`ENTER_GAP`].
    fn into_requests(self) -> Result<Vec<Request>> {
        if let Command::Send {
            id,
            text,
            key: None,
            no_enter: false,
        } = self
        {
            return Ok(vec![
                Request::SendText {
                    id: id.clone(),
                    text: text.join(" "),
                },
                // `\r`, not `\n`: Enter on a real terminal sends a carriage
                // return, and a program reading raw input may ignore a line feed
                // entirely.
                Request::SendText {
                    id,
                    text: "\r".to_string(),
                },
            ]);
        }
        Ok(vec![self.into_request()?])
    }

    fn into_request(self) -> Result<Request> {
        Ok(match self {
            Command::List => Request::List,
            Command::Spawn { agent, cwd, size } => Request::Spawn {
                agent,
                cwd,
                cols: size.map(|(c, _)| c),
                rows: size.map(|(_, r)| r),
            },
            Command::Screen { id } => Request::ReadScreen { id },
            Command::Send {
                id,
                text,
                key,
                no_enter,
            } => {
                let text = match key {
                    Some(name) => {
                        // Every key name resolves to ASCII, so this cannot fail
                        // for a known key, and literal fallback text came in as a
                        // `String` already. The check is for honesty, not hope.
                        String::from_utf8(argus_manifest::key_bytes(&name))
                            .context("key resolved to bytes that are not valid text")?
                    }
                    // Text followed by Enter never reaches here; `into_requests`
                    // splits it into two keystrokes. What is left is text alone.
                    None => {
                        debug_assert!(no_enter, "text + Enter is handled by into_requests");
                        text.join(" ")
                    }
                };
                Request::SendText { id, text }
            }
            Command::Resize { id, cols, rows } => Request::Resize { id, cols, rows },
            Command::Kill { id } => Request::Kill { id },
        })
    }
}

fn print_reply(reply: Reply) {
    match reply {
        Reply::Sessions { mut sessions } => {
            render::by_urgency(&mut sessions);
            print!("{}", render::session_table(&sessions));
        }
        Reply::Session { session } => println!("{}", render::session_line(&session)),
        Reply::Screen { screen } => print!("{}", render::screen(&screen)),
        Reply::Done => {}
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let pipe = cli
        .pipe
        .as_deref()
        .map_or_else(default_pipe_name, pipe_path);
    let requests = cli.command.into_requests()?;

    let result = async {
        let mut client = Client::connect(&pipe).await?;
        let mut reply = Reply::Done;
        for (i, request) in requests.into_iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(ENTER_GAP).await;
            }
            reply = client.call(request).await?;
        }
        Ok::<Reply, ClientError>(reply)
    }
    .await;

    match result {
        Ok(reply) => {
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&reply)?);
            } else {
                print_reply(reply);
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(ClientError::NotRunning(pipe)) => {
            eprintln!("error: the Argus engine is not running (nothing listening on {pipe})");
            eprintln!("hint: start it with `argus-engine`");
            Ok(ExitCode::from(3))
        }
        Err(ClientError::Rpc(error)) => {
            // The engine's message is written for people; the code is printed
            // too because it is what to search for, and what a script matches.
            eprintln!("error: {} [{:?}]", error.message, error.code);
            Ok(ExitCode::FAILURE)
        }
        Err(other) => Err(other.into()),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
