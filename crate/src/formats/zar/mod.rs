mod format;
mod read;
mod write;

#[cfg(fuzzing)]
pub use format::ZarFooter;
pub(crate) use format::{FOOTER_MAGIC, FOOTER_SIZE};
pub(crate) use read::ZarArchiveReader;
#[cfg(fuzzing)]
pub(crate) use read::{build_files, parse_tree_entry};
pub(crate) use write::ZarSession;
