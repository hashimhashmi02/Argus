//! A small client that talks to the engine over its named pipe.
//!
//! This exists so the engine can be driven and debugged by hand -- spawn a
//! session, read its screen, kill it -- long before any UI framework is
//! involved. If the system is not usable from this binary, it is not ready for
//! a UI to be built on top of it.
//!
//! Implemented in step 6. See DESIGN.md.

fn main() {
    println!("argus-cli: not implemented yet -- arrives in step 6. See DESIGN.md.");
}
