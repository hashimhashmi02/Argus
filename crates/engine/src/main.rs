//! The Argus daemon.
//!
//! This is the process that owns every live session. It runs detached from any
//! UI, which is the whole reason the project is split daemon/client: closing
//! the Argus window must never kill a running agent. An agent mid-refactor
//! should survive the user quitting the app, rebooting the UI, or the UI
//! crashing outright.
//!
//! Implemented in step 5, once `argus-protocol` and `argus-control-server`
//! exist for it to serve. See DESIGN.md.

fn main() {
    println!("argus-engine: not implemented yet -- arrives in step 5. See DESIGN.md.");
}
