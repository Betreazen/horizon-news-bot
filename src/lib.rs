//! The parts of the Horizon news bot. `main.rs` wires them into one run:
//! fetch releases, decide what is news, translate it, send it, remember it.

pub mod digest;
pub mod github;
pub mod parse;
pub mod state;
pub mod telegram;
pub mod translate;
