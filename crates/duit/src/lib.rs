//! Option parsing, config files, and the main loop.

pub use args::{Arg, Args};
pub use config::{Config, OptionError, ScanUi};
pub use errors::{
    open_error, read_error, seek_error, spawn_error, stat_error, strerror, write_error,
};

mod args;
mod config;
mod errors;
mod util;
