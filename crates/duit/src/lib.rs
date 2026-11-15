//! Option parsing, config files, and the main loop.

pub use args::{Arg, Args};
pub use config::{
    Color, Config, GraphStyle, OptionError, ScanUi, SharedColumn, SortColumn, SortOrder,
};
pub use errors::{open_error, stat_error, write_error};

mod args;
mod config;
mod errors;
