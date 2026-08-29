use crate::core::attach_xbe::patch_xbe_cert_in_place;
use crate::core::extracted_fs::ExtractedFilesystem;
use crate::core::iso;
use crate::core::iso::probe_source_over;
use crate::core::source::{ImageSource, OwnedSourceReader, ProbedDirectoryTable, SourceReader};
use crate::session::ChunkSource;
use std::io::{Read, Seek, SeekFrom};

/// Extract-time options for patching the root-level `default.xbe`'s
/// certificate before it's written out.
pub(crate) struct XbePatchOptions {
    pub(crate) allowed_media_patch: bool,
    /// Pre-encoded UTF-16LE bytes for the new title. `None` disables
    /// renaming.
    pub(crate) rename_title: Option<Vec<u8>>,
}

/// Where an `ExtractedSession` actually reads its raw file bytes from.
///
/// `Fs` covers both a loose-files source and a `.zar` archive, since
/// `source::open` returns the same `SourceInner::ExtractedFs` variant for
/// both - unpacking a `.zar`'s packed/compressed files is real work, a
/// loose-files source is simply already this shape.
enum ExtractedBacking {
    Image {
        reader: OwnedSourceReader,
        root_offset: u64,
    },
    Fs(Box<ExtractedFilesystem>),
}

/// One file this session will stream out, in whichever terms its
/// `ExtractedBacking` needs to locate it:
///
/// - `image_offset`: absolute byte offset within the image (root offset
///   not yet subtracted) - only meaningful for `ExtractedBacking::Image`.
/// - `fs_index`: the file's position in the source `ExtractedFilesystem::
///   file_entries()` order - only meaningful for `ExtractedBacking::Fs`.
///   Tracked separately from this struct's own position in
///   `ExtractedSession::files`, since `skip_system_update` filtering can
///   shift the latter without changing the former.
struct StreamFile {
    path: String,
    size: u64,
    image_offset: u64,
    fs_index: usize,
}

pub(crate) struct ExtractedSession {
    backing: ExtractedBacking,
    files: Vec<StreamFile>,
    current_index: usize,
    /// Name of the file most recently returned by `next_chunk`.
    last_returned_name: Option<String>,
    current_offset_in_file: u64,
    /// Whether the current file's initial seek has happened yet (there's
    /// one shared reader/filesystem, not a per-file one).
    started: bool,
    xbe_patch: Option<XbePatchOptions>,
    /// `files` index of the root-level `default.xbe`, resolved once at
    /// open time - `None` if there is no root-level `default.xbe` (e.g. a
    /// GoD/XEX source), in which case patching never applies.
    xbe_index: Option<usize>,
    /// Fully-buffered, patched bytes for `xbe_index`'s file plus a read
    /// cursor into them - built lazily on first touch, dropped once fully
    /// streamed out. `default.xbe` is always small, so buffering it whole
    /// avoids the cert straddling two `next_chunk` calls.
    pending_patched: Option<(Vec<u8>, usize)>,
}

/// True if `path`'s root-level, case-insensitive name is `default.xbe`.
fn is_root_default_xbe(path: &str) -> bool {
    !path
        .trim_start_matches(['/', '\\'])
        .replace('\\', "/")
        .contains('/')
        && path.to_ascii_lowercase().ends_with("default.xbe")
}

impl ExtractedSession {
    /// `probed`, when present, is a directory-tree walk a caller already
    /// did on this exact `source` - reused instead of walking again.
    pub(crate) fn open(
        source: Box<dyn ImageSource>,
        skip_system_update: bool,
        xbe_patch: Option<XbePatchOptions>,
        probed: Option<ProbedDirectoryTable>,
    ) -> Result<Self, anyhow::Error> {
        let mut source = source;
        let root_offset = source.image_offset();
        // Scoped so the mutable borrow ends before `source` moves into
        // OwnedSourceReader below.
        let directory_table = if let Some(p) = probed {
            p.directory_table
        } else {
            let probe_reader = SourceReader::new(source.as_mut());
            let detected =
                probe_source_over(probe_reader).map_err(|e| anyhow::anyhow!("extracted: {e:#}"))?;
            detected.directory_table
        };
        let mut raw_files: Vec<(String, u32, u32)> = directory_table
            .entries
            .into_iter()
            .filter(|e| !e.is_directory())
            .map(|e| (e.path, e.sector, e.size))
            .collect();
        if skip_system_update {
            raw_files.retain(|(path, _, _)| {
                !path
                    .trim_start_matches(['/', '\\'])
                    .to_ascii_uppercase()
                    .starts_with("$SYSTEMUPDATE")
            });
        }
        let xbe_index = raw_files
            .iter()
            .position(|(path, _, _)| is_root_default_xbe(path));
        let files = raw_files
            .into_iter()
            .map(|(path, sector, size)| StreamFile {
                path,
                size: u64::from(size),
                image_offset: root_offset + u64::from(sector) * iso::SECTOR_SIZE,
                fs_index: 0, // unused for Image backing
            })
            .collect();
        Ok(Self {
            backing: ExtractedBacking::Image {
                reader: OwnedSourceReader::new(source),
                root_offset,
            },
            files,
            current_index: 0,
            last_returned_name: None,
            current_offset_in_file: 0,
            started: false,
            xbe_patch,
            xbe_index,
            pending_patched: None,
        })
    }

    /// Counterpart to `open()` for a source that's already
    /// `ExtractedFilesystem`-shaped. Streams each file by its stable
    /// `ExtractedFilesystem` index via `read_file_range`, rather than
    /// seeking an `OwnedSourceReader` at an absolute offset like `open()`
    /// does.
    pub(crate) fn open_from_extracted(
        fs: ExtractedFilesystem,
        skip_system_update: bool,
        xbe_patch: Option<XbePatchOptions>,
    ) -> Self {
        let mut raw_files: Vec<(usize, String, u64)> = fs
            .file_entries()
            .into_iter()
            .enumerate()
            .map(|(fs_index, (path, size))| (fs_index, path, size))
            .collect();
        if skip_system_update {
            raw_files.retain(|(_, path, _)| {
                !path
                    .trim_start_matches(['/', '\\'])
                    .to_ascii_uppercase()
                    .starts_with("$SYSTEMUPDATE")
            });
        }
        let xbe_index = raw_files
            .iter()
            .position(|(_, path, _)| is_root_default_xbe(path));
        let files = raw_files
            .into_iter()
            .map(|(fs_index, path, size)| StreamFile {
                path,
                size,
                image_offset: 0, // unused for Fs backing
                fs_index,
            })
            .collect();
        Self {
            backing: ExtractedBacking::Fs(Box::new(fs)),
            files,
            current_index: 0,
            last_returned_name: None,
            current_offset_in_file: 0,
            started: false,
            xbe_patch,
            xbe_index,
            pending_patched: None,
        }
    }

    /// Reads exactly `buf.len()` bytes starting at `offset_in_file`
    /// within `files[idx]`, dispatching to whichever backing this
    /// session was opened from.
    fn read_exact_in_file(
        &mut self,
        idx: usize,
        offset_in_file: u64,
        buf: &mut [u8],
    ) -> Result<(), anyhow::Error> {
        let image_offset = self.files[idx].image_offset;
        let fs_index = self.files[idx].fs_index;
        match &mut self.backing {
            ExtractedBacking::Image {
                reader,
                root_offset,
            } => {
                let seek_pos = image_offset - *root_offset + offset_in_file;
                reader.seek(SeekFrom::Start(seek_pos))?;
                reader.read_exact(buf)?;
                Ok(())
            }
            ExtractedBacking::Fs(fs) => fs.read_file_range(fs_index, offset_in_file, buf),
        }
    }
}

impl ChunkSource for ExtractedSession {
    fn next_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, anyhow::Error> {
        if self.current_index >= self.files.len() {
            return Ok(None);
        }
        let patch_this_entry =
            self.xbe_patch.is_some() && self.xbe_index == Some(self.current_index);
        if patch_this_entry {
            if self.pending_patched.is_none() {
                let idx = self.current_index;
                let size = usize::try_from(self.files[idx].size)
                    .map_err(|e| anyhow::anyhow!("File size exceeds architecture limits: {e}"))?;
                let mut buf = vec![0u8; size];
                self.read_exact_in_file(idx, 0, &mut buf)?;
                let opts = self
                    .xbe_patch
                    .as_ref()
                    .expect("patch_this_entry implies xbe_patch is Some");
                patch_xbe_cert_in_place(
                    &mut buf,
                    opts.allowed_media_patch,
                    opts.rename_title.as_deref(),
                )?;
                self.last_returned_name = Some(self.files[idx].path.clone());
                self.pending_patched = Some((buf, 0));
            }
            let (buf, pos) = self
                .pending_patched
                .as_mut()
                .expect("just populated above if it was None");
            let end = (*pos + max_bytes).min(buf.len());
            let chunk = buf[*pos..end].to_vec();
            *pos = end;
            if *pos >= buf.len() {
                self.pending_patched = None;
                self.current_index += 1;
                self.started = false;
            }
            return Ok(Some(chunk));
        }
        let idx = self.current_index;
        let entry_size = self.files[idx].size;
        if !self.started {
            self.current_offset_in_file = 0;
            self.last_returned_name = Some(self.files[idx].path.clone());
            self.started = true;
        }
        let remaining = entry_size - self.current_offset_in_file;
        let to_read = usize::try_from((max_bytes as u64).min(remaining))
            .expect("remaining is bounded by max_bytes, a usize");
        let mut buf = vec![0u8; to_read];
        self.read_exact_in_file(idx, self.current_offset_in_file, &mut buf)?;
        self.current_offset_in_file += to_read as u64;
        if self.current_offset_in_file >= entry_size {
            self.current_index += 1;
            self.started = false;
        }
        Ok(Some(buf))
    }

    fn is_done(&self) -> bool {
        self.current_index >= self.files.len()
    }

    fn total_units(&self) -> u64 {
        self.files.len() as u64
    }

    fn current_entry_name(&self) -> Option<&str> {
        self.last_returned_name.as_deref()
    }

    fn output_manifest(&self) -> Vec<(String, u64)> {
        self.files
            .iter()
            .map(|f| (f.path.clone(), f.size))
            .collect()
    }
}
