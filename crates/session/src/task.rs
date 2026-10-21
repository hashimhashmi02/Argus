//! The loop that drives one live session.

use std::sync::Arc;
use std::time::Instant;

use argus_manifest::Agent;
use argus_pty::{ConPtyProcess, PtyProcess};
use argus_reducer::{HookSignal, Policy, ScreenMatch, SessionState, Status, Timestamp, reduce};
use argus_terminal_emu::TerminalEmulator;
use tokio::sync::{broadcast, mpsc, watch};

use crate::config::SessionConfig;
use crate::handle::Command;
use crate::snapshot::SessionSnapshot;
use crate::stream::{Repaint, STREAM_DEPTH, StreamItem, Utf8Chunks, Watch};

pub(crate) struct SessionTask {
    agent: Arc<Agent>,
    policy: Policy,
    config: SessionConfig,

    emulator: TerminalEmulator,
    state: SessionState,

    /// `None` once the pseudo-console has been closed.
    ///
    /// Optional rather than always-present because closing it is a deliberate,
    /// ordered step in shutdown, not something to leave to the end of scope.
    process: Option<ConPtyProcess>,

    /// The one real clock read in the whole status pipeline.
    ///
    /// Everything below this line takes time as a parameter. The reducer never
    /// reads a clock, which is what makes its entire test suite run without
    /// sleeping; this is the single place where wall time enters the system.
    start: Instant,

    /// Whether the child has produced output yet.
    ///
    /// ConPTY discards anything written before its startup handshake completes
    /// -- no error, the bytes simply never arrive. See DESIGN.md.
    ready: bool,

    /// Input that arrived before the child was ready.
    pending_input: Vec<u8>,

    exit_code: Option<u32>,

    snapshot: watch::Sender<SessionSnapshot>,
    /// Last published screen, so a repaint is only sent when something changed.
    last_screen: String,

    /// Raw output for watchers. Dropped with the task, which is how a watcher
    /// learns the session has finished.
    output: broadcast::Sender<StreamItem>,
    /// Kept up to date even with nobody watching: a watcher that arrives
    /// between the two halves of a character needs the first half to come
    /// through in front of the second.
    utf8: Utf8Chunks,
}

impl SessionTask {
    /// `start` is passed in rather than read here so that the task and every
    /// `SessionHandle` measure from the *same* instant. `since` is a duration
    /// from `start`, and a handle computes "how long in this status" as its age
    /// minus `since`; two separately-read clocks would make that subtraction
    /// off by however long it took to get from one read to the other.
    pub(crate) fn new(
        agent: Arc<Agent>,
        config: SessionConfig,
        process: ConPtyProcess,
        snapshot: watch::Sender<SessionSnapshot>,
        start: Instant,
    ) -> Self {
        let policy = agent.manifest().timing.apply(Policy::default());
        let emulator = TerminalEmulator::new(config.cols, config.rows);

        Self {
            agent,
            policy,
            emulator,
            state: SessionState::new(Timestamp::ZERO),
            process: Some(process),
            start,
            ready: false,
            pending_input: Vec::new(),
            exit_code: None,
            snapshot,
            last_screen: String::new(),
            output: broadcast::channel(STREAM_DEPTH).0,
            utf8: Utf8Chunks::default(),
            config,
        }
    }

    /// Run until the child exits and the PTY drains.
    pub(crate) async fn run(
        mut self,
        mut bytes: mpsc::Receiver<Vec<u8>>,
        mut commands: mpsc::Receiver<Command>,
    ) {
        let mut ticker = tokio::time::interval(self.config.tick);
        // Default `Burst` behaviour would fire catch-up ticks back to back after
        // any delay, which for us means a flurry of pointless re-evaluations.
        // We only care that time keeps moving, not that no tick is ever missed.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let mut pty_open = true;
        let mut commands_open = true;

        loop {
            let mut hook = None;

            tokio::select! {
                // Biased so output is always drained before a tick is serviced.
                // Without it, a chatty child and a 100ms tick race, and the
                // status could be recomputed against a screen that is one chunk
                // out of date for no reason.
                biased;

                chunk = bytes.recv(), if pty_open => match chunk {
                    Some(chunk) => self.on_bytes(&chunk),
                    None => pty_open = false,
                },

                command = commands.recv(), if commands_open => match command {
                    Some(command) => self.on_command(command),
                    // Every handle was dropped. The session keeps running --
                    // that is the whole point of a daemon outliving its UI --
                    // but nothing can drive it any more.
                    None => commands_open = false,
                },

                _ = ticker.tick() => {}
            }

            // Whatever woke us, the clock has moved, so the status is
            // re-evaluated. This is what "poll with a timeout" buys: a debounce
            // is a claim about elapsed time, and elapsed time passes whether or
            // not the child has anything to say.
            if let Some(code) = self.poll_exit() {
                self.exit_code = Some(code);
                hook = Some(HookSignal::Exited);
            }

            self.evaluate(hook);

            // Done is terminal. Close the console so the reader sees EOF, then
            // keep looping to drain whatever the child wrote on its way out.
            if self.state.status() == Status::Done {
                self.close_pty();
                if !pty_open {
                    break;
                }
            }
        }

        // One last pass so the final screen is published.
        self.evaluate(None);
    }

    /// Feed PTY output to the emulator and honour anything it wants written
    /// back.
    fn on_bytes(&mut self, chunk: &[u8]) {
        let reply = self.emulator.advance(chunk);

        // A terminal is not a passive sink. ConPTY asks where the cursor is and
        // blocks until told; step 1 answered with a hardcoded lie because a raw
        // byte pipe does not know. The emulator does, and this is the line that
        // delivers the honest answer.
        if !reply.is_empty() {
            self.write_now(&reply);
        }

        let text = self.utf8.push(chunk);
        if !text.is_empty() && self.output.receiver_count() > 0 {
            // A send only fails when nobody is subscribed, which was just
            // checked; and if the last watcher left in between, there is
            // nobody to tell.
            let _ = self.output.send(StreamItem::Output(text.into()));
        }

        if !self.ready {
            self.ready = true;
            if !self.pending_input.is_empty() {
                let queued = std::mem::take(&mut self.pending_input);
                self.write_now(&queued);
            }
        }
    }

    fn on_command(&mut self, command: Command) {
        match command {
            Command::Input(bytes) => {
                if self.ready {
                    self.write_now(&bytes);
                } else {
                    // Held rather than dropped. Writing into a pseudo-console
                    // that has not finished its handshake silently discards the
                    // bytes, and a caller has no way to know that happened.
                    self.pending_input.extend_from_slice(&bytes);
                }
            }

            Command::Resize { cols, rows } => {
                self.config.cols = cols;
                self.config.rows = rows;
                self.emulator.resize(cols, rows);
                if let Some(process) = self.process.as_mut() {
                    let _ = process.resize(cols, rows);
                }
                // The emulator has reflowed its grid, so a watcher drawing at
                // the old size no longer matches it. The child redraws for the
                // new size on its own; this only gets watchers to where the
                // emulator is now, so that redraw lands where it should.
                if self.output.receiver_count() > 0 {
                    let _ = self.output.send(StreamItem::Repaint(self.repaint()));
                }
            }

            // Subscribing here, between two reads from the child, is what
            // makes the repaint and the stream line up exactly. See `Watch`.
            Command::Watch(reply) => {
                let _ = reply.send(Watch {
                    repaint: self.repaint(),
                    stream: self.output.subscribe(),
                });
            }

            // Evidence from the agent itself, handed straight to the reducer on
            // the same footing as the screen.
            Command::Hook(signal) => self.evaluate(Some(signal)),

            Command::Kill => {
                if let Some(process) = self.process.as_mut() {
                    let _ = process.kill();
                }
            }
        }
    }

    fn repaint(&self) -> Repaint {
        let (cols, rows) = self.emulator.size();
        Repaint {
            cols,
            rows,
            data: self.emulator.repaint().into(),
        }
    }

    /// Write to the child, ignoring failures.
    ///
    /// A failed write means the child is gone, which the exit poll will notice
    /// on this same iteration. Surfacing it here as well would be a second
    /// report of one event.
    fn write_now(&mut self, bytes: &[u8]) {
        if let Some(process) = self.process.as_mut() {
            let _ = process.write(bytes);
        }
    }

    fn poll_exit(&mut self) -> Option<u32> {
        if self.exit_code.is_some() {
            return None;
        }
        let process = self.process.as_mut()?;
        process.try_wait().ok().flatten().map(|status| status.code)
    }

    /// Classify the screen, fold it into the status, publish if anything moved.
    fn evaluate(&mut self, hook: Option<HookSignal>) {
        let screen = self.emulator.screen();

        // The composition this whole step exists to perform: the emulator says
        // what is on screen, the manifest says what that means, the reducer says
        // what to believe. None of the three knows the other two exist.
        let matched: ScreenMatch = self.agent.classify(screen.lines());
        let now = Timestamp::from_elapsed(self.start.elapsed());
        let next = reduce(&self.state, &matched, hook, now, &self.policy);

        let text = screen.text();
        let changed = next.status() != self.state.status()
            || next.pending() != self.state.pending()
            || next.rule() != self.state.rule()
            || text != self.last_screen;

        self.state = next;

        if changed {
            self.last_screen = text;
            // A send failure means every reader has gone. The session carries on
            // regardless; a daemon does not stop working because nobody is
            // currently looking.
            let _ = self.snapshot.send(SessionSnapshot {
                status: self.state.status(),
                since: self.state.since(),
                rule: self.state.rule().map(str::to_owned),
                pending: self.state.pending(),
                screen,
                pid: self.process.as_ref().and_then(ConPtyProcess::process_id),
                exit_code: self.exit_code,
            });
        }
    }

    /// Close the pseudo-console, which is what finally ends the reader thread.
    ///
    /// On a Unix pty the child exiting closes the slave and the reader sees EOF
    /// for free. ConPTY's output pipe belongs to the *console*, not the child,
    /// so it stays open after the process is gone and the reader blocks forever.
    /// Dropping the master runs `ClosePseudoConsole`, and only the owner can do
    /// that -- so child exit and PTY EOF have to be sequenced by hand.
    fn close_pty(&mut self) {
        self.process.take();
    }
}
