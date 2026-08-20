//! Open tabs in, focus commands out.
//!
//! No OS API exposes tabs, so this needs an extension. Alternatives and why
//! they are closed: DESIGN.md.
//!
//! Off unless configured. `gate` has what a connection must prove.

pub mod base64;
pub mod crypto;
pub mod gate;
pub mod peers;
pub mod protocol;
pub mod server;


