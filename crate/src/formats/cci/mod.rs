mod format;
mod read;
mod write;

#[cfg(fuzzing)]
pub use format::CciHeader;
pub use format::MAGIC;
pub(crate) use read::CciSource;
#[cfg(fuzzing)]
pub(crate) use read::fuzz_parse_header_and_index;
pub(crate) use write::CciSession;
