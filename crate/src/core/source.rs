use crate::core::executable::{TitleExecutionInfo, xbe, xex};
use crate::core::extracted_fs::ExtractedFilesystem;
use crate::core::iso::probe_source_over;
use crate::core::reader::JsReader;
use crate::core::thumbnail;
use crate::core::title::{ContentType, TitleInfo, TitleVersion};
use crate::formats::cci::{CciSource, MAGIC as CCI_MAGIC};
use crate::formats::ciso::{CisoSource, MAGIC as CISO_MAGIC};
use crate::formats::god::GodSource;
use crate::formats::stfs::{MAGIC_CON, MAGIC_LIVE, MAGIC_PIRS, StfsReader};
use crate::formats::xiso::XisoSource;
use crate::formats::zar::{FOOTER_MAGIC, FOOTER_SIZE, ZarArchiveReader};
use crate::game_list;
use crate::utils::js_number_to_u64;
use anyhow::Context;
use js_sys::{Array, Function};
use std::collections::HashMap;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use tsify::Tsify;
use wasm_bindgen::prelude::*;
use wasm_bindgen::{JsCast, JsValue};

/// XDVDFS sector size. See `<https://xboxdevwiki.net/XDVDFS>`.
pub(crate) const SECTOR_SIZE: u64 = 2048;

/// Format-agnostic view onto the XDVDFS bytes of whatever container this
/// input actually is. Everything above this - directory-tree walk,
/// platform detection, sector scanning - goes through `SourceReader`
/// instead, so it never needs to know the concrete container format.
pub(crate) trait ImageSource: Send + Sync {
    /// One `SECTOR_SIZE`-byte sector, relative to `image_offset()`.
    fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), anyhow::Error>;
    /// Arbitrary byte range, relative to the same root - needed since
    /// directory-tree entries aren't sector-aligned.
    fn read_bytes(&mut self, offset: u64, out: &mut [u8]) -> Result<(), anyhow::Error>;
    fn total_sectors(&self) -> u64;
    /// Byte offset of the XDVDFS root within the source's own container.
    /// 0 for CISO/CCI/GOD; one of the XSF/XGD1/XGD2/XGD3 candidate
    /// offsets for raw XISO.
    fn image_offset(&self) -> u64;
    /// The source's own declared content type, when it's knowable
    /// without inferring one from the launch executable - `None` by
    /// default. Only overridden by a GOD source opened with its header;
    /// see `formats::god::read::GodSource::content_type_override`.
    fn content_type_override(&self) -> Option<ContentType> {
        None
    }
    /// The source's own header-embedded Thumbnail Image (0x171A), when
    /// available without XDVDFS positioning - `None` by default.
    fn header_thumbnail(&self) -> Option<&[u8]> {
        None
    }
    /// Same as `header_thumbnail`, for the Title Thumbnail Image
    /// (0x571A).
    fn header_title_thumbnail(&self) -> Option<&[u8]> {
        None
    }
    /// Hints that upcoming reads will be a single large forward linear
    /// pass rather than scattered/revisited access. No-op default -
    /// only `JsReader`-backed sources have anything to switch. See
    /// `JsReader::set_sequential_mode`. Implementations that read out of
    /// source order should NOT override this.
    fn set_sequential_mode(&mut self, _enabled: bool) {}
}

/// Bounds a directory entry's claimed byte length against the source's
/// actual total size before it's used to size an allocation - a
/// corrupted or malicious image could otherwise claim an arbitrarily
/// large `default.xbe`/`default.xex`/thumbnail and OOM the read.
pub(crate) fn validate_entry_size(
    source: &dyn ImageSource,
    sector: u32,
    size: u32,
) -> Result<(), anyhow::Error> {
    let end = u64::from(sector) * SECTOR_SIZE + u64::from(size);
    let available = source.total_sectors() * SECTOR_SIZE;
    anyhow::ensure!(
        end <= available,
        "corrupt or malicious directory entry: claims {size} bytes at sector {sector}, \
         extending past the {available}-byte source"
    );
    Ok(())
}

// SourceReadFn/SourcePart carry a live js_sys::Function, which can't
// implement Serialize/Deserialize, so tsify can't derive these - declared
// here by hand instead.
#[wasm_bindgen(typescript_custom_section)]
const SOURCE_READ_FN_TS: &str = r#"
/**
 * Reads `length` bytes starting at `offset` from the underlying source.
 */
export type SourceReadFn = (offset: number, length: number) => Uint8Array;
/**
 * One file backing a (possibly split) source - a raw XISO can arrive as
 * `game.1.iso`, `game.2.iso`, ... A single-file source is a
 * one-element `SourcePart[]`.
 */
export interface SourcePart {
	name: string;
	size: number;
	readFn: SourceReadFn;
}
"#;

// Typed escape hatches so the generated `.d.ts` shows a real type
// instead of `Function`/`any`. `typescript_type` only affects the type
// checker; both are still plain `JsValue` at the ABI level.
#[wasm_bindgen]
extern "C" {
    // `pub`: appears in the signature of several exported functions in
    // `lib.rs` (`openConversionSession`, `inspectSource`, ...).
    #[wasm_bindgen(typescript_type = "SourceReadFn")]
    pub type SourceReadFnExtern;
    // `parts_from_js` treats `undefined`/`null` as "single-file, fall
    // back to `read_fn`/`file_size`", so this type must admit both.
    #[wasm_bindgen(typescript_type = "SourcePart[] | null | undefined")]
    pub type SourcePartsExtern;
    // `required_parts_from_js` callers have no implicit single-file
    // fallback, so this variant admits neither `null` nor `undefined`.
    #[wasm_bindgen(typescript_type = "SourcePart[]")]
    pub(crate) type SourcePartsRequiredExtern;

    // Named getters for one `SourcePart[]` array entry, so a typo in a
    // field name is a compile error instead of a silent miss on a
    // string literal. Still `Result<JsValue, JsValue>` rather than a
    // concrete type - `part_from_js` turns a missing/wrong-type field
    // into a specific error instead of e.g. a bare f64 getter silently
    // turning a missing `size` into `NaN`.
    #[wasm_bindgen(typescript_type = "SourcePart")]
    type SourcePartEntry;
    #[wasm_bindgen(method, getter, js_name = name, catch)]
    fn name(this: &SourcePartEntry) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(method, getter, js_name = size, catch)]
    fn size(this: &SourcePartEntry) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(method, getter, js_name = readFn, catch)]
    fn read_fn(this: &SourcePartEntry) -> Result<JsValue, JsValue>;
}

/// One file backing a (possibly split) source - a raw XISO can arrive as
/// `game.1.iso`, `game.2.iso`, .... A single-file source is a
/// one-element `Vec<SourcePart>`.
#[derive(Clone)]
pub(crate) struct SourcePart {
    /// Used only for error messages.
    pub(crate) name: String,
    pub(crate) read_fn: Function,
    pub(crate) size: u64,
}

/// Shared `Seek` arithmetic for readers that track their own
/// `position`/`size` (`SourceReader`, `OwnedSourceReader`,
/// `MultiPartReader`). Resolves `pos` against `position`/`size`,
/// rejecting anything before the start.
fn seek_relative(pos: SeekFrom, position: u64, size: u64) -> io::Result<u64> {
    let new_pos = match pos {
        SeekFrom::Start(n) => n.cast_signed(),
        SeekFrom::End(n) => size.cast_signed() + n,
        SeekFrom::Current(n) => position.cast_signed() + n,
    };
    if new_pos < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "seek before start",
        ));
    }
    Ok(new_pos.cast_unsigned())
}

/// Boundary lookup behind `MultiPartReader::locate`, factored out for
/// unit testing without a real `js_sys::Function`. Returns the index of
/// the part containing `pos` and that part's start offset in the
/// logical stream.
fn locate_in(ends: &[u64], pos: u64) -> (usize, u64) {
    let idx = match ends.binary_search(&pos) {
        Ok(i) => i + 1, // pos sits exactly on a boundary -> next part
        Err(i) => i,
    }
    .min(ends.len() - 1);
    let start = if idx == 0 { 0 } else { ends[idx - 1] };
    (idx, start)
}

/// `Read + Seek` over a sequence of `SourcePart`s, presented as one
/// contiguous logical stream starting at `parts[0]`'s first byte.
///
/// Only one part's `JsReader` is held open at a time (`active`), rebuilt
/// when `position` crosses into a different part - avoids holding N live
/// JS file handles for a multi-gigabyte split source.
pub(crate) struct MultiPartReader {
    parts: Vec<SourcePart>,
    /// `ends[i]` = exclusive end offset (in the combined stream) of
    /// `parts[i]`. Cumulative sum of `parts[..].size`.
    ends: Vec<u64>,
    total_size: u64,
    sequential_window: usize,
    active: Option<(usize, JsReader)>,
    position: u64,
    /// Mirrors whichever mode was last requested via
    /// `set_sequential_mode`, so a `JsReader` built fresh in `reader_for`
    /// (on crossing into a different part) starts in the right mode
    /// instead of always defaulting back to `Cached`.
    sequential_mode: bool,
}

impl MultiPartReader {
    pub(crate) fn new(
        parts: Vec<SourcePart>,
        sequential_window: usize,
    ) -> Result<Self, anyhow::Error> {
        anyhow::ensure!(!parts.is_empty(), "at least one part is required");
        let mut ends = Vec::with_capacity(parts.len());
        let mut running = 0u64;
        for part in &parts {
            running += part.size;
            ends.push(running);
        }
        Ok(Self {
            parts,
            ends,
            total_size: running,
            sequential_window,
            active: None,
            position: 0,
            sequential_mode: false,
        })
    }

    /// Switches the underlying `JsReader` between the scattered-read block
    /// cache and the bulk sequential readahead window. Safe to call at
    /// any point, including mid-stream when crossing part boundaries: it
    /// updates the active reader and is remembered so the next rebuild
    /// starts in the correct mode instead of reverting to `Cached`.
    pub(crate) fn set_sequential_mode(&mut self, enabled: bool) {
        self.sequential_mode = enabled;
        if let Some((_, reader)) = &mut self.active {
            reader.set_sequential_mode(enabled);
        }
    }

    /// Index of the part containing logical offset `pos`, and that part's
    /// start offset in the logical stream.
    fn locate(&self, pos: u64) -> (usize, u64) {
        locate_in(&self.ends, pos)
    }

    /// Returns the `JsReader` for `idx`, building it only if not already
    /// active - repeated reads within the same part reuse it and keep
    /// its read-ahead buffer/cache warm.
    fn reader_for(&mut self, idx: usize) -> &mut JsReader {
        if !matches!(&self.active, Some((active_idx, _)) if *active_idx == idx) {
            let part = &self.parts[idx];
            let mut reader = JsReader::new(part.read_fn.clone(), part.size);
            // Sizes the Sequential-mode readahead window; the
            // scattered-read cache uses a fixed internal block size.
            reader.set_sequential_window(self.sequential_window);
            reader.set_sequential_mode(self.sequential_mode);
            self.active = Some((idx, reader));
        }
        &mut self
            .active
            .as_mut()
            .expect("just set to Some(...) above if it wasn't already the active entry")
            .1
    }
}

impl Read for MultiPartReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining_total = self.total_size.saturating_sub(self.position);
        if remaining_total == 0 || buf.is_empty() {
            return Ok(0);
        }
        let (idx, part_start) = self.locate(self.position);
        let part_size = self.parts[idx].size;
        let pos_in_part = self.position - part_start;
        let remaining_in_part = part_size - pos_in_part;
        let remaining_in_part = usize::try_from(remaining_in_part).unwrap_or(usize::MAX);
        let n = buf.len().min(remaining_in_part);
        let reader = self.reader_for(idx);
        // A no-op when pos_in_part is still inside the buffered window.
        reader.seek(SeekFrom::Start(pos_in_part))?;
        let n = reader.read(&mut buf[..n])?;
        self.position += n as u64;
        Ok(n)
    }
}

impl Seek for MultiPartReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.position = seek_relative(pos, self.position, self.total_size)?;
        Ok(self.position)
    }
}

/// `Read + Seek` adapter over `&mut dyn ImageSource`, so any `ImageSource`
/// can be handed to an `R: Read + Seek` consumer without knowing about
/// container formats. Positions are relative to the source's own root
/// (offset 0 == wherever `ImageSource::image_offset()` points).
pub(crate) struct SourceReader<'a> {
    source: &'a mut dyn ImageSource,
    position: u64,
}

impl<'a> SourceReader<'a> {
    pub(crate) fn new(source: &'a mut dyn ImageSource) -> Self {
        Self {
            source,
            position: 0,
        }
    }

    fn size(&self) -> u64 {
        self.source.total_sectors() * SECTOR_SIZE
    }
}

impl Read for SourceReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.size().saturating_sub(self.position);
        let remaining = usize::try_from(remaining).unwrap_or(usize::MAX);
        let n = buf.len().min(remaining);
        if n == 0 {
            return Ok(0);
        }
        self.source
            .read_bytes(self.position, &mut buf[..n])
            .map_err(|e| io::Error::other(format!("{e:#}")))?;
        self.position += n as u64;
        Ok(n)
    }
}

impl Seek for SourceReader<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.position = seek_relative(pos, self.position, self.size())?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemSource(Vec<u8>);

    impl ImageSource for MemSource {
        fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
            self.read_bytes(sector * SECTOR_SIZE, out)
        }

        fn read_bytes(&mut self, offset: u64, out: &mut [u8]) -> Result<(), anyhow::Error> {
            let start = usize::try_from(offset).context("offset too large for this platform")?;
            let end = start + out.len();
            anyhow::ensure!(end <= self.0.len(), "read past end of source");
            out.copy_from_slice(&self.0[start..end]);
            Ok(())
        }

        fn total_sectors(&self) -> u64 {
            self.0.len() as u64 / SECTOR_SIZE
        }

        fn image_offset(&self) -> u64 {
            0
        }
    }

    fn mem_source(sectors: u64) -> MemSource {
        let len = usize::try_from(sectors * SECTOR_SIZE).expect("test size fits in usize");
        let mut data = vec![0u8; len];
        for (i, b) in data.iter_mut().enumerate() {
            *b = u8::try_from(i % 251).expect("i % 251 always fits in u8");
        }
        MemSource(data)
    }

    #[test]
    fn validate_entry_size_accepts_entry_within_source() {
        let source = mem_source(4);
        validate_entry_size(&source, 3, u32::try_from(SECTOR_SIZE).unwrap()).unwrap();
    }

    #[test]
    fn validate_entry_size_rejects_entry_past_source_end() {
        let source = mem_source(4);
        // Same claim, but one byte too many - would read past the source.
        let err =
            validate_entry_size(&source, 3, u32::try_from(SECTOR_SIZE).unwrap() + 1).unwrap_err();
        assert!(err.to_string().contains("extending past"));
    }

    #[test]
    fn validate_entry_size_rejects_overflowing_claim() {
        let source = mem_source(1);
        let err = validate_entry_size(&source, 0, u32::MAX).unwrap_err();
        assert!(err.to_string().contains("extending past"));
    }

    #[test]
    fn read_exact_across_full_source() {
        let mut source = mem_source(4);
        let expected = source.0.clone();
        let mut reader = SourceReader::new(&mut source);
        let mut out = vec![0u8; expected.len()];
        reader.read_exact(&mut out).unwrap();
        assert_eq!(out, expected);
        // exhausted: next read returns Ok(0), not an error
        let mut extra = [0u8; 1];
        assert_eq!(reader.read(&mut extra).unwrap(), 0);
    }

    #[test]
    fn seek_start_current_end_agree() {
        let mut source = mem_source(2);
        let expected = source.0.clone();
        let mut reader = SourceReader::new(&mut source);
        reader.seek(SeekFrom::Start(SECTOR_SIZE)).unwrap();
        let mut a = [0u8; 8];
        reader.read_exact(&mut a).unwrap();
        let sector_size = usize::try_from(SECTOR_SIZE).expect("SECTOR_SIZE fits in usize");
        assert_eq!(&a, &expected[sector_size..sector_size + 8]);
        reader.seek(SeekFrom::Current(-8)).unwrap();
        let mut b = [0u8; 8];
        reader.read_exact(&mut b).unwrap();
        assert_eq!(a, b);
        let end = reader.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(end, expected.len() as u64);
    }

    #[test]
    fn seek_before_start_errors() {
        let mut source = mem_source(1);
        let mut reader = SourceReader::new(&mut source);
        assert!(reader.seek(SeekFrom::Current(-1)).is_err());
    }

    // Three parts of sizes 100, 200, 50 -> ends = [100, 300, 350].
    fn three_part_ends() -> Vec<u64> {
        vec![100, 300, 350]
    }

    #[test]
    fn locate_within_first_part() {
        assert_eq!(locate_in(&three_part_ends(), 0), (0, 0));
        assert_eq!(locate_in(&three_part_ends(), 99), (0, 0));
    }

    #[test]
    fn locate_exactly_on_a_boundary_lands_in_the_next_part() {
        // Position 100 is the first byte after part 0 -> part 1.
        assert_eq!(locate_in(&three_part_ends(), 100), (1, 100));
        assert_eq!(locate_in(&three_part_ends(), 300), (2, 300));
    }

    #[test]
    fn locate_within_middle_and_last_part() {
        assert_eq!(locate_in(&three_part_ends(), 150), (1, 100));
        assert_eq!(locate_in(&three_part_ends(), 349), (2, 300));
    }

    #[test]
    fn locate_at_total_size_clamps_to_last_part() {
        assert_eq!(locate_in(&three_part_ends(), 350), (2, 300));
    }

    #[test]
    fn locate_single_part_always_returns_index_zero() {
        let ends = vec![500u64];
        assert_eq!(locate_in(&ends, 0), (0, 0));
        assert_eq!(locate_in(&ends, 250), (0, 0));
        assert_eq!(locate_in(&ends, 500), (0, 0));
    }

    #[test]
    fn god_open_sorts_shuffled_data_parts_by_numeric_suffix() {
        let names = [
            "Foo.data/Data0002",
            "Foo.data/Data0000",
            "Foo.data/Data0001",
        ];
        let mut sorted_parts: Vec<&str> = names.to_vec();
        sorted_parts.sort_by_key(|name| {
            name.rsplit_once('/')
                .map(|(_, f)| f)
                .unwrap_or(name)
                .strip_prefix("Data")
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(u32::MAX)
        });
        assert_eq!(
            sorted_parts,
            [
                "Foo.data/Data0000",
                "Foo.data/Data0001",
                "Foo.data/Data0002"
            ]
        );
    }

    #[test]
    fn god_data_dir_and_index_matches_the_data_file_prefix_as_case_insensitively_as_its_own_directory_extension_check()
     {
        assert_eq!(
            god_data_dir_and_index("Foo.data/Data0000"),
            Some(("Foo.data", 0))
        );
        assert_eq!(
            god_data_dir_and_index("Foo.data/data0000"),
            Some(("Foo.data", 0)),
            "lowercase \"data0000\" should be recognized the same as \"Data0000\""
        );
        assert_eq!(
            god_data_dir_and_index("Foo.data/DATA0001"),
            Some(("Foo.data", 1)),
            "uppercase \"DATA0001\" should be recognized the same as \"Data0001\""
        );
    }

    #[test]
    fn looks_god_matches_the_data_file_prefix_as_case_insensitively_as_god_data_dir_and_index_does()
    {
        assert!(
            looks_god(&["Foo.data/Data0000".to_owned()]),
            "sanity check: exact-case \"Data0000\" must already be recognized"
        );
        assert!(
            looks_god(&["Foo.data/data0000".to_owned()]),
            "lowercase \"data0000\" should be recognized the same as \"Data0000\", \
             matching god_data_dir_and_index's own case-insensitive prefix check"
        );
        assert!(
            looks_god(&["Foo.data/DATA0000".to_owned()]),
            "uppercase \"DATA0000\" should be recognized the same as \"Data0000\", \
             matching god_data_dir_and_index's own case-insensitive prefix check"
        );
    }

    #[test]
    fn god_part_index_matches_regardless_of_data_prefix_casing() {
        assert_eq!(god_part_index("Data0000"), Some(0));
        assert_eq!(god_part_index("data0000"), Some(0));
        assert_eq!(god_part_index("DATA0001"), Some(1));
        assert_eq!(god_part_index("DaTa0002"), Some(2));
        assert_eq!(god_part_index("NotData0000"), None);
        assert_eq!(god_part_index("Data"), None);
    }

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn looks_extracted_true_for_a_lone_root_xbe() {
        // Documents current, intentionally permissive behavior: a single
        // loose default.xbe with nothing else alongside it is still
        // reported as "looks extracted" - see detect_dir_extracted_from_a_lone_xbe_is_a_known_weak_positive
        // below and the module-level notes on looks_extracted's evidence bar.
        assert!(looks_extracted(&owned(&["default.xbe"])));
    }

    #[test]
    fn looks_extracted_true_for_xbe_alongside_real_game_data() {
        assert!(looks_extracted(&owned(&[
            "default.xbe",
            "media/movie.wmv",
            "media/audio/track.wma",
        ])));
    }

    #[test]
    fn looks_extracted_false_when_no_loose_executable_present() {
        assert!(!looks_extracted(&owned(&["readme.txt", "media/movie.wmv"])));
    }

    #[test]
    fn looks_extracted_false_when_a_loose_iso_is_present() {
        assert!(!looks_extracted(&owned(&["default.xbe", "backup.iso"])));
    }

    #[test]
    fn looks_extracted_false_when_a_loose_cso_is_present() {
        assert!(!looks_extracted(&owned(&["default.xbe", "backup.cso"])));
    }

    #[test]
    fn looks_extracted_false_when_a_loose_cci_is_present() {
        assert!(!looks_extracted(&owned(&["default.xbe", "backup.cci"])));
    }

    #[test]
    fn looks_extracted_false_when_a_loose_zar_is_present() {
        // Regression test for the zar gap: a loose .zar sitting next to
        // default.xbe must disqualify this from being treated as a flat
        // "extracted" filesystem, the same way iso/cso/cci already do -
        // .zar has its own reader (ZarArchiveReader) and should never be
        // silently folded in as an ordinary extracted-fs file.
        assert!(!looks_extracted(&owned(&["default.xbe", "other-game.zar"])));
    }

    #[test]
    fn looks_extracted_false_when_a_loose_zar_is_present_with_xex() {
        assert!(!looks_extracted(&owned(&["default.xex", "other-game.zar"])));
    }

    #[test]
    fn looks_extracted_ignores_a_nested_zar_the_same_way_it_ignores_nested_iso() {
        // The disqualifier only inspects top-level (no '/') entries - a
        // .zar/.iso/.cso/.cci several levels deep is someone else's
        // problem, not evidence against *this* directory being extracted.
        assert!(looks_extracted(&owned(&[
            "default.xbe",
            "extras/bonus.zar",
        ])));
    }

    #[test]
    fn looks_extracted_false_for_a_lone_zar_with_no_executable() {
        assert!(!looks_extracted(&owned(&["other-game.zar"])));
    }

    #[test]
    fn detect_dir_extracted_from_a_lone_xbe_is_a_known_weak_positive() {
        // Same known-weak-signal case as looks_extracted_true_for_a_lone_root_xbe,
        // exercised through the public detect_dir entry point. A single
        // structurally-valid, standalone default.xbe is legitimate for some
        // homebrew, so this intentionally still classifies as Extracted -
        // corroboration/confidence handling belongs downstream, not here.
        assert_eq!(
            detect_dir(&owned(&["default.xbe"])),
            Some(FileType::Extracted)
        );
    }

    #[test]
    fn detect_dir_none_for_a_lone_zar_with_no_executable() {
        assert_eq!(detect_dir(&owned(&["other-game.zar"])), None);
    }

    #[test]
    fn detect_dir_none_for_xbe_next_to_a_loose_zar() {
        assert_eq!(detect_dir(&owned(&["default.xbe", "other-game.zar"])), None);
    }
}

pub(crate) struct OwnedSourceReader {
    source: Box<dyn ImageSource>,
    position: u64,
}

impl OwnedSourceReader {
    pub(crate) fn new(source: Box<dyn ImageSource>) -> Self {
        Self {
            source,
            position: 0,
        }
    }

    fn size(&self) -> u64 {
        self.source.total_sectors() * SECTOR_SIZE
    }

    pub(crate) fn set_sequential_mode(&mut self, enabled: bool) {
        self.source.set_sequential_mode(enabled);
    }
}

impl Read for OwnedSourceReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.size().saturating_sub(self.position);
        let remaining = usize::try_from(remaining).unwrap_or(usize::MAX);
        let n = buf.len().min(remaining);
        if n == 0 {
            return Ok(0);
        }
        self.source
            .read_bytes(self.position, &mut buf[..n])
            .map_err(|e| io::Error::other(format!("{e:#}")))?;
        self.position += n as u64;
        Ok(n)
    }
}

impl Seek for OwnedSourceReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.position = seek_relative(pos, self.position, self.size())?;
        Ok(self.position)
    }
}

pub(crate) struct ProbedDirectoryTable {
    pub(crate) directory_table: crate::core::iso::DirectoryTable,
    pub(crate) title_info: TitleInfo,
}

impl ProbedDirectoryTable {
    fn probe(source: &mut dyn ImageSource) -> Result<Self, anyhow::Error> {
        let reader = SourceReader::new(source);
        let mut iso = probe_source_over(reader)?;
        let title_info = TitleInfo::from_image(&mut iso)?;
        Ok(Self {
            directory_table: iso.directory_table,
            title_info,
        })
    }
}

pub(crate) enum SourceInner {
    Image {
        source: Box<dyn ImageSource>,
        probed: Option<ProbedDirectoryTable>,
    },
    ExtractedFs(Box<ExtractedFilesystem>),
}

impl SourceInner {
    pub(crate) fn probed(&mut self) -> Result<&ProbedDirectoryTable, anyhow::Error> {
        match self {
            SourceInner::Image { source, probed } => {
                if probed.is_none() {
                    *probed = Some(ProbedDirectoryTable::probe(source.as_mut())?);
                }
                Ok(probed.as_ref().expect("just set above"))
            }
            SourceInner::ExtractedFs(_) => {
                anyhow::bail!("extracted sources have no XDVDFS directory table to probe")
            }
        }
    }

    pub(crate) fn into_image_source_with_probe(
        self,
    ) -> Result<(Box<dyn ImageSource>, Option<ProbedDirectoryTable>), anyhow::Error> {
        match self {
            SourceInner::Image { source, probed } => Ok((source, probed)),
            SourceInner::ExtractedFs(_) => {
                anyhow::bail!("extracted sources aren't an ImageSource")
            }
        }
    }
}

#[derive(serde::Deserialize, Tsify)]
#[serde(tag = "format", rename_all = "camelCase")]
pub enum SourceOptions {
    Xiso,
    Ciso,
    Cci,
    God,
    Extracted,
    Zar,
    Stfs,
}

fn god_part_index(file_name: &str) -> Option<u32> {
    let prefix = file_name.get(..4)?;
    if !prefix.eq_ignore_ascii_case("data") {
        return None;
    }
    file_name[4..].parse::<u32>().ok()
}

pub(crate) fn open(
    opts: &SourceOptions,
    parts: Vec<SourcePart>,
    sequential_window: Option<usize>,
) -> Result<SourceInner, anyhow::Error> {
    let window = sequential_window.unwrap_or(crate::core::reader::DEFAULT_SEQ_WINDOW);
    match opts {
        SourceOptions::Xiso => {
            anyhow::ensure!(!parts.is_empty(), "xiso: at least one part is required");
            let source: Box<dyn ImageSource> = if parts.len() == 1 {
                let part = parts.into_iter().next().unwrap();
                Box::new(XisoSource::open(part.read_fn, part.size, window)?)
            } else {
                Box::new(XisoSource::open_multi_part(parts, window)?)
            };
            Ok(SourceInner::Image {
                source,
                probed: None,
            })
        }
        SourceOptions::Ciso => {
            anyhow::ensure!(!parts.is_empty(), "ciso: at least one part is required");
            Ok(SourceInner::Image {
                source: Box::new(CisoSource::open(parts, window)?),
                probed: None,
            })
        }
        SourceOptions::Cci => Ok(SourceInner::Image {
            source: Box::new(CciSource::open(parts, window)?),
            probed: None,
        }),
        SourceOptions::God => {
            let mut sorted_parts = Vec::with_capacity(parts.len());
            let mut header_part = None;
            for part in parts {
                let file_name = part
                    .name
                    .rsplit_once('/')
                    .map_or(part.name.as_str(), |(_, file_name)| file_name);
                if god_part_index(file_name).is_some() {
                    sorted_parts.push(part);
                } else if header_part.is_none() {
                    header_part = Some(part);
                } else {
                    anyhow::bail!(
                        "god: unexpected extra non-Data part {file_name:?} - only one header part is supported"
                    );
                }
            }
            sorted_parts.sort_by_key(|p| {
                let file_name = p
                    .name
                    .rsplit_once('/')
                    .map_or(p.name.as_str(), |(_, file_name)| file_name);
                god_part_index(file_name).unwrap_or(u32::MAX)
            });
            Ok(SourceInner::Image {
                source: Box::new(GodSource::open(sorted_parts, window, header_part)?),
                probed: None,
            })
        }
        SourceOptions::Extracted => Ok(SourceInner::ExtractedFs(Box::new(
            ExtractedFilesystem::new(parts)?,
        ))),
        SourceOptions::Zar => {
            anyhow::ensure!(
                parts.len() == 1,
                "zar: source doesn't support multiple parts, got {}",
                parts.len()
            );
            let part = parts.into_iter().next().unwrap();
            let archive = ZarArchiveReader::open(part.read_fn, part.size)?;
            Ok(SourceInner::ExtractedFs(Box::new(
                ExtractedFilesystem::new_from_zar(archive),
            )))
        }
        SourceOptions::Stfs => {
            anyhow::ensure!(
                parts.len() == 1,
                "stfs: source doesn't support multiple parts, got {}",
                parts.len()
            );
            let part = parts.into_iter().next().unwrap();
            let archive = StfsReader::open(part.read_fn, part.size)?;
            Ok(SourceInner::ExtractedFs(Box::new(
                ExtractedFilesystem::new_from_stfs(archive),
            )))
        }
    }
}

fn part_from_js(entry: &SourcePartEntry) -> Result<SourcePart, anyhow::Error> {
    let name = entry
        .name()
        .map_err(|_| anyhow::anyhow!("source part is missing `name`"))?
        .as_string()
        .ok_or_else(|| anyhow::anyhow!("source part `name` must be a string"))?;
    let size = js_number_to_u64(
        entry
            .size()
            .map_err(|_| anyhow::anyhow!("source part is missing `size`"))?
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("source part `size` must be a number"))?,
        "source part `size`",
    )?;
    let read_fn: Function = entry
        .read_fn()
        .map_err(|_| anyhow::anyhow!("source part is missing `readFn`"))?
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("source part `readFn` must be a function"))?;
    Ok(SourcePart {
        name,
        read_fn,
        size,
    })
}

fn array_from_js(source_parts: &JsValue) -> Result<Vec<SourcePart>, anyhow::Error> {
    let array: Array = source_parts
        .clone()
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("sourceParts must be an array"))?;
    array
        .iter()
        .map(|entry| part_from_js(entry.unchecked_ref()))
        .collect()
}

pub(crate) fn parts_from_js(
    source_parts: &SourcePartsExtern,
    fallback_read_fn: &Function,
    fallback_size: u64,
) -> Result<Vec<SourcePart>, anyhow::Error> {
    let source_parts: &JsValue = source_parts.as_ref();
    if source_parts.is_undefined() || source_parts.is_null() {
        return Ok(vec![SourcePart {
            name: String::new(),
            read_fn: fallback_read_fn.clone(),
            size: fallback_size,
        }]);
    }
    let parts = array_from_js(source_parts)?;
    anyhow::ensure!(!parts.is_empty(), "sourceParts array must not be empty");
    Ok(parts)
}

pub(crate) fn required_parts_from_js(
    source_parts: &SourcePartsRequiredExtern,
) -> Result<Vec<SourcePart>, anyhow::Error> {
    let source_parts: &JsValue = source_parts.as_ref();
    anyhow::ensure!(
        !source_parts.is_undefined() && !source_parts.is_null(),
        "expected an array of source parts, got null/undefined"
    );
    let parts = array_from_js(source_parts)?;
    anyhow::ensure!(!parts.is_empty(), "source parts array must not be empty");
    Ok(parts)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub enum FileType {
    Xiso,
    Ciso,
    Cci,
    God,
    Extracted,
    Zar,
    Stfs,
}

fn looks_extracted(entries: &[String]) -> bool {
    let mut exe_found = false;
    for path in entries.iter().filter(|p| !p.contains('/')) {
        let ext = Path::new(path.as_str())
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("iso" | "cso" | "cci" | "zar") => return false,
            Some("xbe" | "xex") => exe_found = true,
            _ => {}
        }
    }
    exe_found
}

fn looks_god(entries: &[String]) -> bool {
    entries.iter().any(|path| {
        let parts: Vec<&str> = path.split('/').collect();
        parts.len() <= 4
            && parts
                .last()
                .is_some_and(|f| f.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("Data")))
            && parts.len() >= 2
            && Path::new(parts[parts.len() - 2])
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("data"))
    })
}

pub(crate) fn detect_dir(entries: &[String]) -> Option<FileType> {
    if looks_god(entries) {
        return Some(FileType::God);
    }
    if looks_extracted(entries) {
        return Some(FileType::Extracted);
    }
    None
}

pub(crate) struct GodCandidate {
    pub(crate) data_dir: String,
    pub(crate) parts: Vec<(u32, SourcePart)>,
}

fn god_data_dir_and_index(path: &str) -> Option<(&str, u32)> {
    let (dir, file) = path.rsplit_once('/')?;
    let prefix = file.get(..4)?;
    if !prefix.eq_ignore_ascii_case("data") {
        return None;
    }
    let index: u32 = file[4..].parse().ok()?;
    let parent_is_data_dir = Path::new(dir)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("data"));
    parent_is_data_dir.then_some((dir, index))
}

pub(crate) fn partition_god_candidates(
    parts: Vec<SourcePart>,
) -> (Vec<GodCandidate>, Vec<SourcePart>) {
    let mut by_dir: HashMap<String, Vec<(u32, SourcePart)>> = HashMap::new();
    let mut leftover = Vec::new();
    for part in parts {
        match god_data_dir_and_index(&part.name) {
            Some((dir, index)) => by_dir
                .entry(dir.to_owned())
                .or_default()
                .push((index, part)),
            None => leftover.push(part),
        }
    }
    let candidates = by_dir
        .into_iter()
        .map(|(data_dir, mut indexed)| {
            indexed.sort_by_key(|(index, _)| *index);
            GodCandidate {
                data_dir,
                parts: indexed,
            }
        })
        .collect();
    (candidates, leftover)
}

pub(crate) fn god_candidate_is_contiguous(candidate: &GodCandidate) -> bool {
    candidate
        .parts
        .iter()
        .enumerate()
        .all(|(position, (index, _))| *index as usize == position)
}

#[derive(Debug, Clone, serde::Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub title_id: String,
    pub content_type: ContentType,
    pub version: TitleVersion,
    pub detected_title: Option<String>,
    pub disc_number: u8,
    pub disc_count: u8,
    #[tsify(type = "Uint8Array | undefined")]
    #[serde(with = "serde_bytes", default)]
    pub thumbnail: Option<Vec<u8>>,
    #[tsify(type = "Uint8Array | undefined")]
    #[serde(with = "serde_bytes", default)]
    pub title_thumbnail: Option<Vec<u8>>,
}

impl SourceInfo {
    fn from_title_info(title_info: &TitleInfo) -> Self {
        let title_id = title_info.execution_info.title_id;
        Self {
            title_id: format!("{title_id:08X}"),
            content_type: title_info.content_type,
            version: title_info.version(),
            detected_title: game_list::find_title_by_id(title_id),
            disc_number: title_info.execution_info.disc_number,
            disc_count: title_info.execution_info.disc_count,
            thumbnail: None,
            title_thumbnail: None,
        }
    }
}

pub(crate) fn disc_suffixed_title(base: &str, disc_number: u8, disc_count: u8) -> String {
    if disc_count > 1 {
        format!("{base} (Disc {disc_number}/{disc_count})")
    } else {
        base.to_owned()
    }
}

pub(crate) fn detect(read_fn: Function, file_size: u64) -> Result<FileType, anyhow::Error> {
    anyhow::ensure!(
        file_size >= 4,
        "file is too small to be a disc image ({file_size} bytes)"
    );
    let mut reader = JsReader::new(read_fn.clone(), file_size);
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    if magic == CISO_MAGIC {
        Ok(FileType::Ciso)
    } else if magic == CCI_MAGIC {
        Ok(FileType::Cci)
    } else if magic == MAGIC_CON || magic == MAGIC_LIVE || magic == MAGIC_PIRS {
        Ok(FileType::Stfs)
    } else if file_size > FOOTER_SIZE as u64 && zar_footer_magic_matches(read_fn, file_size)? {
        Ok(FileType::Zar)
    } else {
        Ok(FileType::Xiso)
    }
}

fn zar_footer_magic_matches(read_fn: Function, file_size: u64) -> Result<bool, anyhow::Error> {
    let mut reader = JsReader::new(read_fn, file_size);
    reader.seek(SeekFrom::End(-4))?;
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    Ok(u32::from_be_bytes(magic) == FOOTER_MAGIC)
}

pub(crate) fn inspect_source(
    opened: &mut SourceInner,
    include_thumbnail: bool,
) -> Result<SourceInfo, anyhow::Error> {
    let SourceInner::Image { source, .. } = opened else {
        anyhow::bail!("inspect_source: expected an image-backed source");
    };
    let content_type_override = source.content_type_override();
    let header_thumbnail = include_thumbnail
        .then(|| source.header_thumbnail().map(<[u8]>::to_vec))
        .flatten();
    let header_title_thumbnail = include_thumbnail
        .then(|| source.header_title_thumbnail().map(<[u8]>::to_vec))
        .flatten();
    let probed = opened.probed()?;
    let mut title_info = probed.title_info.clone();
    let directory_table = probed.directory_table.clone();

    let is_xex = title_info.content_type == ContentType::GamesOnDemand;
    if let Some(content_type) = content_type_override {
        title_info.content_type = content_type;
    }

    let mut info = SourceInfo::from_title_info(&title_info);
    if include_thumbnail {
        let SourceInner::Image { source, .. } = opened else {
            unreachable!("checked above")
        };
        info.thumbnail =
            thumbnail_from_entries(source.as_mut(), &directory_table, is_xex)?.or(header_thumbnail);
        info.title_thumbnail = header_title_thumbnail;
    }
    Ok(info)
}

fn thumbnail_from_entries(
    source: &mut dyn ImageSource,
    directory_table: &crate::core::iso::DirectoryTable,
    is_xex: bool,
) -> Result<Option<Vec<u8>>, anyhow::Error> {
    let entry_name = if is_xex { "default.xex" } else { "default.xbe" };

    let Some(entry) = directory_table.find(entry_name) else {
        return Ok(None);
    };
    validate_entry_size(source, entry.sector, entry.size)?;
    let mut buf = vec![0u8; entry.size as usize];
    source.read_bytes(u64::from(entry.sector) * SECTOR_SIZE, &mut buf)?;
    if is_xex {
        thumbnail::thumbnail_from_xex(&buf)
    } else {
        thumbnail::thumbnail_from_xbe(&buf)
    }
}

pub(crate) fn title_info_from_exe_bytes(
    exe_bytes: &[u8],
    is_xex: bool,
) -> Result<TitleInfo, anyhow::Error> {
    if is_xex {
        let header =
            xex::XexHeader::read(Cursor::new(exe_bytes)).context("error reading default.xex")?;
        let execution_info = header
            .fields
            .execution_info
            .context("no execution info in default.xex header")?;
        Ok(TitleInfo {
            content_type: ContentType::GamesOnDemand,
            execution_info,
        })
    } else {
        let header =
            xbe::XbeHeader::read(Cursor::new(exe_bytes)).context("error reading default.xbe")?;
        let execution_info = header
            .fields
            .execution_info
            .context("no execution info in default.xbe header")?;
        Ok(TitleInfo {
            content_type: ContentType::XboxOriginal,
            execution_info,
        })
    }
}

fn placeholder_execution_info() -> TitleExecutionInfo {
    TitleExecutionInfo {
        media_id: 0,
        version: 0,
        base_version: 0,
        title_id: 0,
        platform: 0,
        executable_type: 0,
        disc_number: 1,
        disc_count: 1,
        save_game_id: 0,
    }
}

pub(crate) fn inspect_extracted(
    fs: &mut ExtractedFilesystem,
    include_thumbnail: bool,
) -> Result<SourceInfo, anyhow::Error> {
    let content_type = fs.stfs_content_type();
    let exe_probe = fs.read_launch_executable();

    let mut title_info = match &exe_probe {
        Ok((exe_bytes, is_xex)) => title_info_from_exe_bytes(exe_bytes, *is_xex)?,
        Err(e) => {
            let executable_required = match content_type {
                Some(ct) => ct.requires_launch_executable(),
                None => true,
            };
            if executable_required {
                return Err(anyhow::anyhow!(
                    "failed to resolve launch executable: {e:#}"
                ));
            }
            TitleInfo {
                content_type: content_type.unwrap_or(ContentType::GamesOnDemand),
                execution_info: placeholder_execution_info(),
            }
        }
    };

    if let Some(content_type) = content_type {
        title_info.content_type = content_type;
    }

    let mut info = SourceInfo::from_title_info(&title_info);
    if include_thumbnail {
        info.thumbnail = match &exe_probe {
            Ok((exe_bytes, true)) => thumbnail::thumbnail_from_xex(exe_bytes)?,
            Ok((exe_bytes, false)) => thumbnail::thumbnail_from_xbe(exe_bytes)?,
            Err(_) => None,
        }
        .or_else(|| fs.stfs_thumbnail().map(<[u8]>::to_vec));
        info.title_thumbnail = fs.stfs_title_thumbnail().map(<[u8]>::to_vec);
    }
    Ok(info)
}
