//! The filesystem scan.

pub use exclude::{DirectoryPatterns, ExcludePatterns, Excluded};
pub use scan::{ScanError, ScanOptions, scan};

mod exclude;
mod scan;
