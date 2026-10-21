//! `argus-cli`: drive the engine by hand.
//!
//! This exists so the engine can be exercised and debugged before any UI does:
//! if the system is not usable from here, it is not ready for a UI to be built
//! on top of it. It also does what building a client always does to a protocol
//! -- the first gap it found is why `SessionInfo` now carries `status_for_ms`.
//!
//! Every command is one request over the named pipe and one reply, except
//! `watch`, which follows a session until it ends.
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

use std::io::Write as _;
use std::process::ExitCode;

use anyhow::{Context, Result};
use argus_control_server::{Client, ClientError, Watcher, default_pipe_name, pipe_path};
use argus_protocol::{Reply, Request, WatchEvent};
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

    /// Approve whatever a session is asking, with that agent's own approve key.
    ///
    /// The engine looks the key up in the agent's manifest -- Enter for one
    /// agent, `y` for another -- so this is the same command for all of them.
    Approve {
        /// Session id, or any prefix naming exactly one session.
        id: String,
    },

    /// Decline whatever a session is asking, with that agent's own deny key.
    Deny {
        /// Session id, or any prefix naming exactly one session.
        id: String,
    },

    /// List the agents this engine can spawn.
    Agents,

    /// Change a session's terminal size.
    Resize { id: String, cols: u16, rows: u16 },

    /// Mirror a session's terminal here, read-only, until it ends.
    ///
    /// Draws the session's screen, colours and all, then follows it live. Stop
    /// with Ctrl+C. Best in a terminal at least as large as the session's. With
    /// `--json`, prints each event the engine sends as one line of JSON.
    Watch {
        /// Session id, or any prefix naming exactly one session.
        id: String,
    },

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

/// What a command does over the pipe.
enum Action {
    /// One request, one reply.
    Call(Request),
    /// Type a line and press Enter as a separate keystroke. See
    /// [`Client::send_line`] for why that has to be two writes, not one.
    Line { id: String, text: String },
    /// Follow a session's terminal until it ends.
    Watch { id: String },
}

/// What came back.
enum Outcome {
    Reply(Reply),
    /// A watch, already written out as it arrived.
    Streamed,
}

impl Command {
    fn into_action(self) -> Result<Action> {
        Ok(Action::Call(match self {
            Command::List => Request::List,
            Command::Agents => Request::Agents,
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
                key: None,
                no_enter: false,
            } => {
                return Ok(Action::Line {
                    id,
                    text: text.join(" "),
                });
            }
            Command::Send {
                id,
                text,
                key,
                no_enter: _,
            } => {
                let text = match key {
                    // Every key name resolves to ASCII, so this cannot fail for a
                    // known key, and literal fallback text came in as a `String`
                    // already. The check is for honesty, not hope.
                    Some(name) => String::from_utf8(argus_manifest::key_bytes(&name))
                        .context("key resolved to bytes that are not valid text")?,
                    None => text.join(" "),
                };
                Request::SendText { id, text }
            }
            Command::Approve { id } => Request::Respond { id, approve: true },
            Command::Deny { id } => Request::Respond { id, approve: false },
            Command::Resize { id, cols, rows } => Request::Resize { id, cols, rows },
            Command::Kill { id } => Request::Kill { id },
            Command::Watch { id } => return Ok(Action::Watch { id }),
        }))
    }
}

/// What leaves the terminal usable after a mirror: attributes reset, the main
/// screen back, the cursor shown, and every input mode the session may have
/// switched on switched off again.
const RESTORE: &str = "\x1b[0m\x1b[?1049l\x1b[?25h\x1b[?1l\x1b>\x1b[?2004l\x1b[?1000l\
                       \x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1006l\r\n";

/// Write a watched session to stdout until it ends or Ctrl+C is pressed.
async fn mirror(mut watcher: Watcher, json: bool) -> Result<(), ClientError> {
    let mut out = std::io::stdout();
    let result = async {
        loop {
            let event = tokio::select! {
                event = watcher.next() => event?,
                _ = tokio::signal::ctrl_c() => None,
            };
            let Some(event) = event else {
                return Ok(());
            };
            if json {
                let line = serde_json::to_string(&event).map_err(std::io::Error::other)?;
                writeln!(out, "{line}")?;
            } else {
                match &event {
                    WatchEvent::Repaint { data, .. } | WatchEvent::Output { data } => {
                        out.write_all(data.as_bytes())?;
                    }
                    WatchEvent::Ended => {}
                }
            }
            out.flush()?;
            if event == WatchEvent::Ended {
                return Ok(());
            }
        }
    }
    .await;
    if !json {
        // Restored however the mirror stopped, including on an error.
        let _ = out.write_all(RESTORE.as_bytes());
        let _ = out.flush();
    }
    result
}

fn print_reply(reply: Reply) {
    match reply {
        Reply::Sessions { mut sessions } => {
            render::by_urgency(&mut sessions);
            print!("{}", render::session_table(&sessions));
        }
        Reply::Session { session } => println!("{}", render::session_line(&session)),
        Reply::Screen { screen } => print!("{}", render::screen(&screen)),
        Reply::Agents { agents } => print!("{}", render::agent_table(&agents)),
        // Only ever the answer to `watch`, which streams instead of printing
        // a reply.
        Reply::Done | Reply::Watching => {}
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let pipe = cli
        .pipe
        .as_deref()
        .map_or_else(default_pipe_name, pipe_path);
    let action = cli.command.into_action()?;

    let result = async {
        let mut client = Client::connect(&pipe).await?;
        match action {
            Action::Call(request) => client.call(request).await.map(Outcome::Reply),
            Action::Line { id, text } => client
                .send_line(&id, &text)
                .await
                .map(|()| Outcome::Reply(Reply::Done)),
            Action::Watch { id } => {
                let watcher = client.watch(&id).await?;
                mirror(watcher, cli.json).await.map(|()| Outcome::Streamed)
            }
        }
    }
    .await;

    match result {
        Ok(Outcome::Streamed) => Ok(ExitCode::SUCCESS),
        Ok(Outcome::Reply(reply)) => {
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
