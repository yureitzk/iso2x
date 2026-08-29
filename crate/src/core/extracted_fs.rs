use crate::core::reader::JsReader;
use crate::core::source::SourcePart;
use crate::core::title::ContentType;
use crate::core::writers::SliceWriter;
use crate::formats::stfs::{AvatarItemMetadata, InstallerMetadata, StfsReader, VideoMetadata};
use crate::formats::zar::ZarArchiveReader;
use crate::utils::is_safe_path_component;
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom};
use xdvdfs::write::fs::{FileEntry, FileType, FilesystemCopier, FilesystemHierarchy, PathRef};

#[derive(Debug)]
pub(crate) struct ExtractedFsError(pub anyhow::Error);

impl core::fmt::Display for ExtractedFsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for ExtractedFsError {}

impl From<anyhow::Error> for ExtractedFsError {
    fn from(e: anyhow::Error) -> Self {
        Self(e)
    }
}

impl From<io::Error> for ExtractedFsError {
    fn from(e: io::Error) -> Self {
        Self(e.into())
    }
}

/// Strips a leading slash and normalizes backslashes to forward slashes.
fn normalize(name: &str) -> String {
    name.trim_start_matches(['/', '\\']).replace('\\', "/")
}

fn validate_names(names: &[&str]) -> Result<(), anyhow::Error> {
    anyhow::ensure!(
        !names.is_empty(),
        "extracted: at least one file is required"
    );
    let mut seen = HashSet::new();
    for name in names {
        anyhow::ensure!(
            !name.is_empty(),
            "extracted: every part must have a non-empty relative path"
        );
        let normalized = normalize(name);
        // Unlike a ZAR/STFS name-table entry, this is a full path - check
        // each `/`-split segment with the same rule `safe_path` applies per-entry.
        for component in normalized.split('/') {
            anyhow::ensure!(
                is_safe_path_component(component),
                "extracted: unsafe path component {component:?} in {name:?}"
            );
        }
        anyhow::ensure!(
            seen.insert(normalized),
            "extracted: duplicate path {name:?}"
        );
    }
    Ok(())
}

/// Lists the direct children of `dir_path` (or root, if `dir_is_root`)
/// from a flat `(name, size)` listing.
fn read_dir_over(entries: &[(&str, u64)], dir_is_root: bool, dir_path: &str) -> Vec<FileEntry> {
    let prefix = if dir_is_root {
        String::new()
    } else {
        format!("{}/", normalize(dir_path))
    };
    let mut children: HashMap<String, FileEntry> = HashMap::new();
    for (name, size) in entries {
        let path = normalize(name);
        let Some(rest) = path.strip_prefix(prefix.as_str()) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        match rest.split_once('/') {
            Some((first, _)) => {
                children.entry(first.to_string()).or_insert(FileEntry {
                    name: first.to_string(),
                    file_type: FileType::Directory,
                    len: 0,
                });
            }
            None => {
                children.insert(
                    rest.to_string(),
                    FileEntry {
                        name: rest.to_string(),
                        file_type: FileType::File,
                        len: *size,
                    },
                );
            }
        }
    }
    children.into_values().collect()
}

/// Root-level default.xex/default.xbe precedence (case-insensitive; xex
/// wins even if xbe was seen earlier).
fn find_launch_executable(names: &[&str]) -> Option<(usize, bool)> {
    let mut found: Option<(usize, bool)> = None;
    for (i, name) in names.iter().enumerate() {
        let path = normalize(name);
        if path.contains('/') {
            continue;
        }
        let lower = path.to_ascii_lowercase();
        if lower == "default.xex" {
            return Some((i, true));
        }
        if lower == "default.xbe" {
            found = Some((i, false));
        }
    }
    found
}

/// Where an `ExtractedFilesystem`'s file bytes come from.
enum Backing {
    /// One `js_sys::Function` read callback per file (a drag-and-dropped
    /// folder).
    Parts {
        parts: Vec<SourcePart>,
        /// Only one part's `JsReader` open at a time.
        active: Option<(usize, JsReader)>,
    },
    /// A parsed `.zar` archive.
    Zar(ZarArchiveReader),
    /// A parsed STFS (LIVE/PIRS/CON) package. Boxed: at 896 bytes it's by
    /// far the largest variant (next is `Zar` at 264), so every `Backing`
    /// would otherwise pay for its size just to be a valid enum.
    Stfs(Box<StfsReader>),
}

/// A filesystem view over a flat `Vec<SourcePart>`, a parsed `.zar`
/// archive, or a parsed STFS package - see `Backing`. Every path is a
/// forward-slash relative path.
pub(crate) struct ExtractedFilesystem {
    backing: Backing,
    /// Per-file byte substitutions, keyed by `file_entries()` index - see
    /// `override_file`. Checked ahead of `backing` in `read_exact_at`.
    overrides: HashMap<usize, Vec<u8>>,
}

impl ExtractedFilesystem {
    /// No `sequential_window` parameter: `ExtractedFilesystem` isn't an
    /// `ImageSource` and exposes no way to enter Sequential mode, so
    /// every read through `Backing::Parts` (directory listing,
    /// launch-executable read, `copy_file_in`) is a scattered Cached-mode
    /// read against `JsReader`'s fixed internal cache block size - there's
    /// no readahead window to size.
    pub(crate) fn new(parts: Vec<SourcePart>) -> Result<Self, anyhow::Error> {
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        validate_names(&names)?;
        Ok(Self {
            backing: Backing::Parts {
                parts,
                active: None,
            },
            overrides: HashMap::new(),
        })
    }

    pub(crate) fn new_from_zar(archive: ZarArchiveReader) -> Self {
        Self {
            backing: Backing::Zar(archive),
            overrides: HashMap::new(),
        }
    }

    pub(crate) fn new_from_stfs(archive: StfsReader) -> Self {
        Self {
            backing: Backing::Stfs(Box::new(archive)),
            overrides: HashMap::new(),
        }
    }

    /// Flat (normalized-path, size) listing of every file, in a stable,
    /// index-aligned order matching `read_exact_at`'s `idx`.
    fn file_entries_impl(&self) -> Vec<(String, u64)> {
        match &self.backing {
            Backing::Parts { parts, .. } => {
                parts.iter().map(|p| (normalize(&p.name), p.size)).collect()
            }
            Backing::Zar(archive) => archive.file_entries(),
            Backing::Stfs(archive) => archive.file_entries(),
        }
    }

    pub(crate) fn file_entries(&self) -> Vec<(String, u64)> {
        self.file_entries_impl()
    }

    /// Content type from the source STFS header, when backed by one -
    /// `None` otherwise, or if unrecognized. Lets an stfs->stfs
    /// conversion preserve e.g. `ArcadeGame` instead of relying on the
    /// executable-based heuristic.
    pub(crate) fn stfs_content_type(&self) -> Option<ContentType> {
        match &self.backing {
            Backing::Stfs(archive) => archive.content_type(),
            _ => None,
        }
    }

    /// Same field as `stfs_content_type`, but unfiltered - `Some(raw)`
    /// even for values `ContentType` doesn't recognize.
    pub(crate) fn stfs_raw_content_type(&self) -> Option<u32> {
        match &self.backing {
            Backing::Stfs(archive) => Some(archive.raw_content_type()),
            _ => None,
        }
    }

    /// Console ID (0x36C) from the source STFS header.
    pub(crate) fn stfs_console_id(&self) -> Option<[u8; 5]> {
        match &self.backing {
            Backing::Stfs(archive) => Some(*archive.console_id()),
            _ => None,
        }
    }

    /// Profile ID / XUID (0x371) from the source STFS header.
    pub(crate) fn stfs_profile_id(&self) -> Option<[u8; 8]> {
        match &self.backing {
            Backing::Stfs(archive) => Some(*archive.profile_id()),
            _ => None,
        }
    }

    /// Online Creator XUID (0x3AD) from the source STFS header.
    pub(crate) fn stfs_online_creator(&self) -> Option<[u8; 8]> {
        match &self.backing {
            Backing::Stfs(archive) => Some(*archive.online_creator()),
            _ => None,
        }
    }

    /// Device ID (0x3FD) from the source STFS header.
    pub(crate) fn stfs_device_id(&self) -> Option<[u8; 20]> {
        match &self.backing {
            Backing::Stfs(archive) => Some(*archive.device_id()),
            _ => None,
        }
    }

    /// Display Name (0x411) from the source STFS header, when present.
    /// Lets an stfs->stfs conversion preserve the source's own name
    /// instead of falling back to a game-list lookup by title ID.
    pub(crate) fn stfs_display_name(&self) -> Option<String> {
        match &self.backing {
            Backing::Stfs(archive) => archive.display_name().map(str::to_owned),
            _ => None,
        }
    }

    /// `AvatarItem`-only structured metadata (0x3D9: subcategory/
    /// colorizable/GUID/skeleton version) from the source STFS header,
    /// when present. Lets an `AvatarItem` stfs->stfs conversion preserve
    /// this region instead of silently zeroing it out.
    pub(crate) fn stfs_avatar_item_metadata(&self) -> Option<AvatarItemMetadata> {
        match &self.backing {
            Backing::Stfs(archive) => archive.avatar_item_metadata(),
            _ => None,
        }
    }

    /// `Video`-only structured metadata (0x3D9: series/season IDs,
    /// season/episode numbers) from the source STFS header, when
    /// present. Lets a `Video` stfs->stfs conversion preserve this
    /// region instead of silently zeroing it out, mirroring
    /// `stfs_avatar_item_metadata`.
    pub(crate) fn stfs_video_metadata(&self) -> Option<VideoMetadata> {
        match &self.backing {
            Backing::Stfs(archive) => archive.video_metadata(),
            _ => None,
        }
    }

    /// Installer trailer (0x971A: installer type/version info, or an
    /// in-progress download's resume state) from the source STFS
    /// header, when present. Lets a system/title-update stfs->stfs
    /// conversion preserve this region instead of silently dropping it.
    pub(crate) fn stfs_installer_metadata(&self) -> Option<InstallerMetadata> {
        match &self.backing {
            Backing::Stfs(archive) => archive.installer_metadata().cloned(),
            _ => None,
        }
    }

    /// License table entries 1..16 (offset 0x23C) from the source STFS
    /// header, verbatim. Entry 0 is excluded - see
    /// `StfsReader::license_entries`.
    pub(crate) fn stfs_license_entries(&self) -> Option<[u8; 0xF0]> {
        match &self.backing {
            Backing::Stfs(archive) => Some(*archive.license_entries()),
            _ => None,
        }
    }

    /// Thumbnail Image (0x171A) from the source STFS header, when
    /// present and valid PNG.
    pub(crate) fn stfs_thumbnail(&self) -> Option<&[u8]> {
        match &self.backing {
            Backing::Stfs(archive) => archive.thumbnail(),
            _ => None,
        }
    }

    /// Title Thumbnail Image (0x571A) from the source STFS header, same
    /// conditions as `stfs_thumbnail`.
    pub(crate) fn stfs_title_thumbnail(&self) -> Option<&[u8]> {
        match &self.backing {
            Backing::Stfs(archive) => archive.title_thumbnail(),
            _ => None,
        }
    }

    fn find(&self, path: &str) -> Option<usize> {
        self.file_entries_impl()
            .iter()
            .position(|(name, _)| name == path)
    }

    /// Replaces the bytes served for `path` for every subsequent read.
    /// Same-size swaps only.
    ///
    /// # Errors
    ///
    /// If `path` isn't in this filesystem's listing, or `bytes.len()`
    /// doesn't match that entry's declared size.
    pub(crate) fn override_file(
        &mut self,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), anyhow::Error> {
        let idx = self
            .find(path)
            .ok_or_else(|| anyhow::anyhow!("extracted: no file at {path:?} to override"))?;
        let declared_size = self.file_entries_impl()[idx].1;
        anyhow::ensure!(
            bytes.len() as u64 == declared_size,
            "extracted: override for {path:?} is {} bytes, expected {declared_size}",
            bytes.len()
        );
        self.overrides.insert(idx, bytes);
        Ok(())
    }

    fn read_exact_at(
        &mut self,
        idx: usize,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), anyhow::Error> {
        if let Some(bytes) = self.overrides.get(&idx) {
            let start = usize::try_from(offset)
                .map_err(|e| anyhow::anyhow!("extracted: override read offset too large: {e}"))?;
            let end = start
                .checked_add(buf.len())
                .filter(|&end| end <= bytes.len())
                .ok_or_else(|| anyhow::anyhow!("extracted: override read past end of override"))?;
            buf.copy_from_slice(&bytes[start..end]);
            return Ok(());
        }
        match &mut self.backing {
            Backing::Parts { parts, active } => {
                if !matches!(active, Some((active_idx, _)) if *active_idx == idx) {
                    let part = &parts[idx];
                    let reader = JsReader::new(part.read_fn.clone(), part.size);
                    *active = Some((idx, reader));
                }
                let reader = &mut active
                    .as_mut()
                    .expect("just set to Some(...) above if it wasn't already the active entry")
                    .1;
                reader.seek(SeekFrom::Start(offset))?;
                reader.read_exact(buf)?;
                Ok(())
            }
            Backing::Zar(archive) => archive.read_file_range(idx, offset, buf),
            Backing::Stfs(archive) => archive.read_file_range(idx, offset, buf),
        }
    }

    /// Locates the root-level default.xbe/default.xex and returns its
    /// full bytes plus whether it's a .xex (true) or .xbe (false).
    ///
    /// `entries[idx].1` (the declared size) is untrusted - it can come
    /// from unverified archive metadata or a caller-supplied
    /// `SourcePart.size`. `vec![0u8; size]` would abort the whole wasm
    /// module with a "capacity overflow" panic on a corrupted/lying size
    /// near `isize::MAX`; `try_reserve_exact` turns that into a normal
    /// `Err` instead.
    pub(crate) fn read_launch_executable(&mut self) -> Result<(Vec<u8>, bool), anyhow::Error> {
        let entries = self.file_entries_impl();
        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        let (idx, is_xex) = find_launch_executable(&names)
            .ok_or_else(|| anyhow::anyhow!("extracted: no default.xbe/default.xex at root"))?;
        // usize is 32 bits on this crate's wasm32 target.
        let size = usize::try_from(entries[idx].1).map_err(|_| {
            anyhow::anyhow!("extracted: launch executable too large for this platform")
        })?;
        let mut buf: Vec<u8> = Vec::new();
        buf.try_reserve_exact(size).map_err(|e| {
            anyhow::anyhow!(
                "extracted: launch executable declares {size} bytes, \
                 which doesn't fit in memory: {e}"
            )
        })?;
        buf.resize(size, 0);
        self.read_exact_at(idx, 0, &mut buf)?;
        Ok((buf, is_xex))
    }

    /// Used by `formats::zar`, which streams each file in small pieces.
    pub(crate) fn read_file_range(
        &mut self,
        idx: usize,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), anyhow::Error> {
        self.read_exact_at(idx, offset, buf)
    }
}

impl FilesystemHierarchy for ExtractedFilesystem {
    type Error = ExtractedFsError;

    fn read_dir(&mut self, dir: PathRef<'_>) -> Result<Vec<FileEntry>, Self::Error> {
        let dir_path = dir.to_string();
        let owned_entries = self.file_entries_impl();
        let entries: Vec<(&str, u64)> = owned_entries
            .iter()
            .map(|(name, size)| (name.as_str(), *size))
            .collect();
        Ok(read_dir_over(&entries, dir.is_root(), &dir_path))
    }
}

impl FilesystemCopier<SliceWriter> for ExtractedFilesystem {
    type Error = ExtractedFsError;

    fn copy_file_in(
        &mut self,
        src: PathRef<'_>,
        dest: &mut SliceWriter,
        input_offset: u64,
        output_offset: u64,
        size: u64,
    ) -> Result<u64, Self::Error> {
        let path = normalize(&src.to_string());
        let idx = self
            .find(&path)
            .ok_or_else(|| anyhow::anyhow!("extracted: no such file {path:?}"))?;
        let file_size = self.file_entries_impl()[idx].1;
        let to_copy = usize::try_from(size.min(file_size.saturating_sub(input_offset)))
            .map_err(|_| anyhow::anyhow!("extracted: copy size too large for this platform"))?;
        let mut buf = vec![0u8; to_copy];
        self.read_exact_at(idx, input_offset, &mut buf)?;
        // SliceWriter only implements xdvdfs's BlockDeviceWrite, not
        // io::Write/Seek.
        xdvdfs::blockdev::BlockDeviceWrite::write(dest, output_offset, &buf)?;
        Ok(size)
    }
}

impl FilesystemCopier<[u8]> for ExtractedFilesystem {
    type Error = ExtractedFsError;

    fn copy_file_in(
        &mut self,
        src: PathRef<'_>,
        dest: &mut [u8],
        input_offset: u64,
        output_offset: u64,
        size: u64,
    ) -> Result<u64, Self::Error> {
        let path = normalize(&src.to_string());
        let idx = self
            .find(&path)
            .ok_or_else(|| anyhow::anyhow!("extracted: no such file {path:?}"))?;
        let file_size = self.file_entries_impl()[idx].1;
        let to_copy = usize::try_from(size.min(file_size.saturating_sub(input_offset)))
            .map_err(|_| anyhow::anyhow!("extracted: copy size too large for this platform"))?;

        let output_offset = usize::try_from(output_offset)
            .map_err(|_| anyhow::anyhow!("extracted: output offset too large for this platform"))?;
        let size_usize = usize::try_from(size)
            .map_err(|_| anyhow::anyhow!("extracted: copy size too large for this platform"))?;
        let limit = output_offset
            .checked_add(size_usize)
            .filter(|&end| end <= dest.len())
            .ok_or_else(|| anyhow::anyhow!("extracted: copy_file_in write out of bounds"))?;
        let write_end = output_offset + to_copy;

        self.read_exact_at(idx, input_offset, &mut dest[output_offset..write_end])?;
        dest[write_end..limit].fill(0);

        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_leading_forward_slash() {
        assert_eq!(normalize("/default.xbe"), "default.xbe");
    }

    #[test]
    fn normalize_strips_leading_backslash() {
        assert_eq!(normalize("\\default.xbe"), "default.xbe");
    }

    #[test]
    fn normalize_converts_backslashes_to_forward_slashes() {
        assert_eq!(normalize("dir\\sub\\file.bin"), "dir/sub/file.bin");
    }

    #[test]
    fn normalize_handles_combined_leading_and_internal_backslashes() {
        assert_eq!(normalize("\\dir\\sub\\file.bin"), "dir/sub/file.bin");
    }

    #[test]
    fn normalize_leaves_already_clean_relative_path_untouched() {
        assert_eq!(normalize("dir/sub/file.bin"), "dir/sub/file.bin");
    }

    #[test]
    fn normalize_of_empty_string_is_empty() {
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn validate_names_rejects_empty_list() {
        let err = validate_names(&[]).unwrap_err();
        assert!(err.to_string().contains("at least one file is required"));
    }

    #[test]
    fn validate_names_rejects_empty_path() {
        let err = validate_names(&["default.xbe", ""]).unwrap_err();
        assert!(err.to_string().contains("non-empty relative path"));
    }

    #[test]
    fn validate_names_rejects_duplicate_after_normalization() {
        let err = validate_names(&["/a.txt", "a.txt"]).unwrap_err();
        assert!(err.to_string().contains("duplicate path"));
    }

    #[test]
    fn validate_names_rejects_duplicate_via_backslash_normalization() {
        let err = validate_names(&["dir\\file.bin", "dir/file.bin"]).unwrap_err();
        assert!(err.to_string().contains("duplicate path"));
    }

    #[test]
    fn validate_names_accepts_distinct_paths() {
        assert!(validate_names(&["default.xbe", "dir/file.bin", "dir/other.bin"]).is_ok());
    }

    #[test]
    fn validate_names_rejects_leading_traversal() {
        let err = validate_names(&["../evil.bin"]).unwrap_err();
        assert!(err.to_string().contains("unsafe path component"));
    }

    #[test]
    fn validate_names_rejects_traversal_in_a_middle_segment() {
        let err = validate_names(&["dir/../../evil.bin"]).unwrap_err();
        assert!(err.to_string().contains("unsafe path component"));
    }

    #[test]
    fn validate_names_rejects_backslash_traversal() {
        let err = validate_names(&["..\\evil.bin"]).unwrap_err();
        assert!(err.to_string().contains("unsafe path component"));
    }

    #[test]
    fn validate_names_rejects_bare_dot_segment() {
        let err = validate_names(&["dir/./evil.bin"]).unwrap_err();
        assert!(err.to_string().contains("unsafe path component"));
    }

    #[test]
    fn read_dir_root_lists_immediate_files_and_folds_nested_into_one_directory_entry() {
        let entries = [
            ("default.xbe", 100u64),
            ("media/movie.wmv", 200u64),
            ("media/audio/track.wma", 50u64),
        ];
        let mut result = read_dir_over(&entries, true, "/");
        result.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "default.xbe");
        assert!(matches!(result[0].file_type, FileType::File));
        assert_eq!(result[0].len, 100);
        assert_eq!(result[1].name, "media");
        assert!(matches!(result[1].file_type, FileType::Directory));
        assert_eq!(result[1].len, 0);
    }

    #[test]
    fn read_dir_nested_lists_only_that_directorys_direct_children() {
        let entries = [
            ("default.xbe", 100u64),
            ("media/movie.wmv", 200u64),
            ("media/audio/track.wma", 50u64),
        ];
        let mut result = read_dir_over(&entries, false, "/media");
        result.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "audio");
        assert!(matches!(result[0].file_type, FileType::Directory));
        assert_eq!(result[1].name, "movie.wmv");
        assert!(matches!(result[1].file_type, FileType::File));
        assert_eq!(result[1].len, 200);
    }

    #[test]
    fn read_dir_deeper_nesting_still_only_one_level_at_a_time() {
        let entries = [("media/audio/track.wma", 50u64)];
        let result = read_dir_over(&entries, false, "/media/audio");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "track.wma");
        assert!(matches!(result[0].file_type, FileType::File));
        assert_eq!(result[0].len, 50);
    }

    #[test]
    fn read_dir_of_nonexistent_directory_is_empty() {
        let entries = [("default.xbe", 100u64)];
        let result = read_dir_over(&entries, false, "/does-not-exist");
        assert!(result.is_empty());
    }

    #[test]
    fn read_dir_root_ignores_nothing_when_every_file_is_top_level() {
        let entries = [("default.xbe", 1u64), ("default.xex", 2u64)];
        let result = read_dir_over(&entries, true, "/");
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn find_launch_executable_prefers_xex_when_both_present() {
        let names = ["default.xbe", "default.xex"];
        assert_eq!(find_launch_executable(&names), Some((1, true)));
    }

    #[test]
    fn find_launch_executable_prefers_xex_even_when_xbe_comes_first_in_list() {
        let names = ["default.xex", "default.xbe"];
        assert_eq!(find_launch_executable(&names), Some((0, true)));
    }

    #[test]
    fn find_launch_executable_falls_back_to_xbe_when_no_xex() {
        let names = ["readme.txt", "default.xbe"];
        assert_eq!(find_launch_executable(&names), Some((1, false)));
    }

    #[test]
    fn find_launch_executable_is_case_insensitive() {
        let names = ["DEFAULT.XBE"];
        assert_eq!(find_launch_executable(&names), Some((0, false)));
    }

    #[test]
    fn find_launch_executable_ignores_nested_files() {
        let names = ["sub/default.xex", "default.xbe"];
        assert_eq!(find_launch_executable(&names), Some((1, false)));
    }

    #[test]
    fn find_launch_executable_returns_none_when_neither_present() {
        let names = ["readme.txt", "media/movie.wmv"];
        assert_eq!(find_launch_executable(&names), None);
    }

    #[test]
    fn find_launch_executable_returns_none_for_empty_list() {
        let names: [&str; 0] = [];
        assert_eq!(find_launch_executable(&names), None);
    }
}
