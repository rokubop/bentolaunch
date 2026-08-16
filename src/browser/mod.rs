//! The browser bridge: open tabs in, focus commands out.
//!
//! No OS API exposes browser tabs, so this needs an extension. The alternatives
//! are closed for reasons recorded in DESIGN.md: UI Automation on the tab strip
//! has no URLs, the DevTools port needs a launch flag, and the profile files are
//! locked and off limits.
//!
//! Off unless configured. See `gate` for what a connection has to prove.

pub mod gate;
pub mod protocol;
pub mod server;


