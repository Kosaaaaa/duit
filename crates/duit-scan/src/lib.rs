//! The filesystem scan.

pub use exclude::{DirectoryPatterns, ExcludePatterns, Excluded};
pub use scan::{ScanError, ScanOptions, scan, stat_at};

mod exclude;
mod scan;
mod threads;
