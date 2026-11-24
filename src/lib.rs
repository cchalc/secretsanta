//! `santa`: a Secret Santa pairing and mailing tool.
//!
//! Architecture: a pure *functional core* (`domain`, `roster`, `rules`,
//! `solver`, `accommodate`, `seal`, `html`, `message`, `send`) wrapped by an *imperative shell*
//! (`cli`, `main.rs`) that owns all I/O. Randomness, the clock and email
//! delivery (`mailer`) are passed into the core explicitly; mailers stamp each
//! message's `Date` at the moment it is sent.

pub mod accommodate;
pub mod cli;
pub mod domain;
pub mod files;
pub mod html;
pub mod mailer;
pub mod message;
pub mod roster;
pub mod rules;
pub mod seal;
pub mod send;
pub mod solver;
pub mod template;
