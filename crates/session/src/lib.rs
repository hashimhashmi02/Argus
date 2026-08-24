//! One live agent process: pty + terminal-emu + manifest + reducer, tied
//! together and driven by its own `tokio` task.
//!
//! The loop polls PTY output *with a timeout* rather than doing a blocking
//! read. A blocking read would park the task until the child says something,
//! which means a silent child freezes the reducer's sense of time and its
//! debounce timers never fire. Polling with a timeout keeps the clock moving
//! even when the process has nothing to say.
//!
//! Implemented in step 4. See DESIGN.md.
