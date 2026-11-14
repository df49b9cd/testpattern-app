//! Chromecast (Google Cast): finds devices on the LAN (mDNS
//! `_googlecast._tcp`, and without it — routers that drop multicast — by
//! address, `probe.rs`), starts Google's Default Media Receiver on one and has
//! it play the current item from the relay (`relay.rs`) — never the provider
//! URL, which carries the account's credentials.
//!
//! One cast session at a time, run by a task that owns the device
//! connection; the UI drives it with `cast_*` commands and follows
//! `cast://status`. While a session runs, everything the user plays goes to
//! the TV (the frontend sends it here instead of to `play`).

pub mod castv2;
mod commands;
mod discovery;
mod probe;
mod relay;
mod session;
mod types;

pub use commands::*;

pub use types::*;
