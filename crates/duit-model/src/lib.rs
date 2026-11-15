//! Entry types, the `Sink` trait, and the in-memory tree.

pub use sink::Sink;
pub use stat::{EntryType, Extended, Special, Stat};

mod sink;
mod stat;
