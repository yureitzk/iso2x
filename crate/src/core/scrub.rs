use crate::core::iso;
use serde::Deserialize;
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use tsify::Tsify;

/// XDVDFS sector size. `<https://free60.org/System-Software/Systems/GDFX>`
pub(crate) const SECTOR_SIZE: u64 = 2048;

/// Volume descriptor sector. `<https://free60.org/System-Software/Systems/GDFX>`
/// Structural metadata, not a `DirectoryEntry`, so `mark_directory_table_sectors`
/// never discovers it on its own.
const VOLUME_DESCRIPTOR_SECTOR: u64 = 0x20;

/// Standard redump sector totals for original Xbox discs, with
/// (`REDUMP_TOTAL_SECTORS`) or without (`REDUMP_GAME_SECTORS`) the video
/// partition. `find_security_sectors` only scans discs matching one of
/// these.
const REDUMP_VIDEO_SECTORS: u64 = 0x30600;
const REDUMP_TOTAL_SECTORS: u64 = 0x003A_4D50;
const REDUMP_GAME_SECTORS: u64 = REDUMP_TOTAL_SECTORS - REDUMP_VIDEO_SECTORS;

/// Upper bound for the security-sector scan.
const SECURITY_SCAN_END_SECTOR: u64 = 0x0034_5B60;

/// Length of the zeroed, unclaimed sector run that marks the security region.
const SECURITY_RUN_LEN: u64 = 0x1000;

/// Detected from the launch executable's extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    /// default.xbe
    Ogx,
    /// default.xex
    X360,
}

/// Result of scanning a disc's directory tree for used sectors and platform.
pub(crate) struct ScrubInfo {
    /// Sectors (relative to `root_offset`) holding real file or
    /// directory-table data; never zeroed.
    pub(crate) used_sectors: HashSet<u64>,
    /// Root-relative byte offset one past the last used byte.
    pub(crate) max_end: u64,
    /// Xbox vs Xbox 360; used to skip interior zeroing on X360 images.
    pub(crate) platform: Platform,
}

/// ciso/cci/god's shared write-mode enum. Wire values are lowercase
/// strings via `#[serde(rename_all = "camelCase")]`.
///
/// `Full` never needs a sector scan, so `plan_direct` only handles `None`
/// and `Partial`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default, Tsify)]
#[serde(rename_all = "camelCase")]
pub enum ScrubMode {
    /// Straight sector copy, no trim, no zero, no directory scan.
    None,
    /// Trims trailing padding and zeroes interior gaps in one pass. On
    /// original Xbox (OGX) images this also tries to leave the disc's
    /// security-sector region alone; on Xbox 360 images interior gaps are
    /// never zeroed, only trimmed.
    Partial,
    /// Full reauthor via a fresh XDVDFS rebuild. Slowest, but independent
    /// of source layout. Default when `mode` is omitted.
    #[default]
    Full,
}

/// Marks a `[sector, sector + size)` byte region (root-relative) as used
/// and folds its end into `max_end`.
fn mark_region(
    sector: u32,
    size: u32,
    root_offset: u64,
    used_sectors: &mut HashSet<u64>,
    max_end: &mut u64,
) {
    let start = root_offset + u64::from(sector) * SECTOR_SIZE;
    let end = start + u64::from(size);
    *max_end = (*max_end).max(end);

    let start_sector = (start - root_offset) / SECTOR_SIZE;
    let end_sector = (end - root_offset).div_ceil(SECTOR_SIZE);
    used_sectors.extend(start_sector..end_sector);
}

/// Marks every directory table's own sectors (root table plus every
/// subdirectory's table) as used. A directory entry's `sector`/`size`
/// *is* its own subdirectory table's region, so this is a flat filter -
/// no recursion needed.
fn mark_directory_table_sectors(
    directory_table: &iso::DirectoryTable,
    root_offset: u64,
    used_sectors: &mut HashSet<u64>,
    max_end: &mut u64,
) {
    mark_region(
        directory_table.root_sector,
        directory_table.root_size,
        root_offset,
        used_sectors,
        max_end,
    );

    for entry in directory_table.entries.iter().filter(|e| e.is_directory()) {
        mark_region(entry.sector, entry.size, root_offset, used_sectors, max_end);
    }
}

fn case_insensitive_contains(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// Finds the disc's launch executable (default.xbe or default.xex) and
/// classifies the platform by its extension.
fn detect_platform(entries: &[iso::DirectoryEntry]) -> Result<Platform, anyhow::Error> {
    let exe = entries
        .iter()
        .filter(|e| !e.is_directory())
        .find(|e| {
            let name = e.path.rsplit('/').next().unwrap_or(e.path.as_str());
            case_insensitive_contains(name, "default.xex")
                || case_insensitive_contains(name, "default.xbe")
        })
        .ok_or_else(|| {
            anyhow::anyhow!("no launch executable (default.xbe/default.xex) found in image")
        })?;

    let name = exe.path.rsplit('/').next().unwrap_or(exe.path.as_str());
    if case_insensitive_contains(name, ".xex") {
        Ok(Platform::X360)
    } else if case_insensitive_contains(name, ".xbe") {
        Ok(Platform::Ogx)
    } else {
        anyhow::bail!("unknown platform: launch executable has neither .xbe nor .xex extension")
    }
}

/// Marks every file's sectors and every directory table's sectors as used,
/// protects the volume descriptor sector, and detects the platform - all
/// in one pass over the flat entry list.
pub(crate) fn scan(
    directory_table: &iso::DirectoryTable,
    root_offset: u64,
    file_size: u64,
) -> Result<ScrubInfo, anyhow::Error> {
    let mut used_sectors: HashSet<u64> = HashSet::new();
    let mut max_end = root_offset;
    used_sectors.insert(VOLUME_DESCRIPTOR_SECTOR);

    for entry in directory_table.entries.iter().filter(|e| !e.is_directory()) {
        mark_region(
            entry.sector,
            entry.size,
            root_offset,
            &mut used_sectors,
            &mut max_end,
        );
    }

    mark_directory_table_sectors(
        directory_table,
        root_offset,
        &mut used_sectors,
        &mut max_end,
    );

    let full_sectors = file_size.saturating_sub(root_offset).div_ceil(SECTOR_SIZE);
    if let Some(&max_used_sector) = used_sectors.iter().max()
        && max_used_sector >= full_sectors
    {
        anyhow::bail!(
            "corrupt image: used sector {max_used_sector} is beyond the image's sector count"
        );
    }

    let platform = detect_platform(&directory_table.entries)?;

    Ok(ScrubInfo {
        used_sectors,
        max_end,
        platform,
    })
}

/// Finds the OGX security-sector region so a partial scrub can leave it
/// alone. Only standard redump-sized images are scanned; anything else
/// returns `None` and the caller falls back to `used_sectors`.
///
/// `full_sectors` must be the untrimmed sector count - the security region
/// can sit past the last file/dir sector `scan` finds.
pub(crate) fn find_security_sectors<R: Read + Seek>(
    reader: &mut R,
    root_offset: u64,
    full_sectors: u64,
    used_sectors: &HashSet<u64>,
) -> Result<Option<HashSet<u64>>, anyhow::Error> {
    if full_sectors != REDUMP_GAME_SECTORS && full_sectors != REDUMP_TOTAL_SECTORS {
        return Ok(None);
    }

    // Don't scan past a truncated image.
    let end_sector = SECURITY_SCAN_END_SECTOR.min(full_sectors.saturating_sub(1));
    scan_for_zero_run(reader, root_offset, end_sector, used_sectors)
}

/// Scans for a contiguous, unclaimed run of exactly `SECURITY_RUN_LEN`
/// zeroed sectors.
fn scan_for_zero_run<R: Read + Seek>(
    reader: &mut R,
    root_offset: u64,
    end_sector: u64,
    used_sectors: &HashSet<u64>,
) -> Result<Option<HashSet<u64>>, anyhow::Error> {
    let mut buf = vec![0u8; usize::try_from(SECTOR_SIZE)?];
    let mut run_start: Option<u64> = None;

    for sector_index in 0..=end_sector {
        reader.seek(SeekFrom::Start(root_offset + sector_index * SECTOR_SIZE))?;
        reader.read_exact(&mut buf)?;

        let is_candidate = buf.iter().all(|&b| b == 0) && !used_sectors.contains(&sector_index);

        match (is_candidate, run_start) {
            (true, None) => run_start = Some(sector_index),
            (false, Some(start)) => {
                let end = sector_index - 1;
                if end - start == SECURITY_RUN_LEN - 1 {
                    return Ok(Some((start..=end).collect()));
                }
                run_start = None;
            }
            _ => {}
        }
    }

    Ok(None)
}

/// Sector count + zero-mask for a non-repack (direct) backend.
///
/// `None` is a straight passthrough. `Partial` derives the trimmed length
/// and an interior zero-mask from one `scan` call, and on OGX images also
/// looks for the security-sector region via `find_security_sectors` so
/// it's protected from re-zeroing.
pub(crate) fn plan_direct<R: Read + Seek>(
    mode: ScrubMode,
    directory_table: &iso::DirectoryTable,
    root_offset: u64,
    file_size: u64,
    reader: &mut R,
) -> Result<(u64, Option<HashSet<u64>>), anyhow::Error> {
    match mode {
        ScrubMode::Full => unreachable!("Full uses the Rebuild backend, not plan_direct"),
        ScrubMode::None => {
            let full_sectors = (file_size - root_offset).div_ceil(SECTOR_SIZE);
            Ok((full_sectors, None))
        }
        ScrubMode::Partial => {
            let info = scan(directory_table, root_offset, file_size)?;
            let mut protected = info.used_sectors.clone();
            let mut trimmed_len = info.max_end - root_offset;

            // Only OGX images have a security-sector region.
            if info.platform == Platform::Ogx {
                let full_sectors = (file_size - root_offset).div_ceil(SECTOR_SIZE);
                if let Some(security) =
                    find_security_sectors(reader, root_offset, full_sectors, &info.used_sectors)?
                {
                    // The security region can sit past scan()'s max_end -
                    // extend the trim point so it isn't cut off before the
                    // zero-mask can protect it.
                    if let Some(&max_sec) = security.iter().max() {
                        trimmed_len = trimmed_len.max((max_sec + 1) * SECTOR_SIZE);
                    }
                    protected.extend(security);
                }
            }

            let total_sectors = trimmed_len.div_ceil(SECTOR_SIZE);
            let zero_sectors = if info.platform == Platform::X360 {
                None
            } else {
                Some(
                    (0..total_sectors)
                        .filter(|s| !protected.contains(s))
                        .collect(),
                )
            };

            Ok((total_sectors, zero_sectors))
        }
    }
}

#[cfg(test)]
mod platform_tests {
    use super::*;
    use crate::core::iso::DirectoryEntry;
    use crate::core::iso::directory_table::DirectoryEntryAttributes;

    fn entry(path: &str) -> DirectoryEntry {
        DirectoryEntry {
            attributes: DirectoryEntryAttributes::ARCHIVE,
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            sector: 0,
            size: 0,
        }
    }

    #[test]
    fn detects_ogx_from_default_xbe() {
        assert_eq!(
            detect_platform(&[entry("default.xbe")]).unwrap(),
            Platform::Ogx
        );
    }

    #[test]
    fn detects_x360_from_default_xex() {
        assert_eq!(
            detect_platform(&[entry("default.xex")]).unwrap(),
            Platform::X360
        );
    }

    #[test]
    fn detection_is_case_insensitive() {
        assert_eq!(
            detect_platform(&[entry("DEFAULT.XEX")]).unwrap(),
            Platform::X360
        );
    }

    #[test]
    fn errors_when_no_launch_executable_present() {
        assert!(detect_platform(&[entry("readme.txt")]).is_err());
    }
}

#[cfg(test)]
mod security_sector_tests {
    use super::*;
    use std::io::Cursor;

    fn fake_image(total_sectors: u64, security_start: u64) -> Vec<u8> {
        let mut data = vec![0xABu8; (total_sectors * SECTOR_SIZE) as usize];
        let start_byte = (security_start * SECTOR_SIZE) as usize;
        let end_byte = ((security_start + SECURITY_RUN_LEN) * SECTOR_SIZE) as usize;
        for b in &mut data[start_byte..end_byte] {
            *b = 0;
        }
        data
    }

    #[test]
    fn finds_run_of_exact_length() {
        let security_start = 100u64;
        let end_sector = security_start + SECURITY_RUN_LEN + 10;
        let image = fake_image(end_sector + 1, security_start);
        let mut reader = Cursor::new(image);
        let used = HashSet::new();

        let found = scan_for_zero_run(&mut reader, 0, end_sector, &used).unwrap();
        let expected: HashSet<u64> = (security_start..security_start + SECURITY_RUN_LEN).collect();
        assert_eq!(found, Some(expected));
    }

    #[test]
    fn used_sector_inside_zero_run_breaks_it() {
        let security_start = 100u64;
        let end_sector = security_start + SECURITY_RUN_LEN + 10;
        let image = fake_image(end_sector + 1, security_start);
        let mut reader = Cursor::new(image);

        let mut used = HashSet::new();
        used.insert(security_start + 10);

        let found = scan_for_zero_run(&mut reader, 0, end_sector, &used).unwrap();
        assert_eq!(found, None);
    }

    #[test]
    fn non_standard_size_is_never_scanned() {
        let mut reader = Cursor::new(vec![0u8; 1]);
        let used = HashSet::new();
        let non_standard_size = REDUMP_GAME_SECTORS + 1;

        let found = find_security_sectors(&mut reader, 0, non_standard_size, &used).unwrap();
        assert_eq!(found, None);
    }
}

#[cfg(test)]
mod root_far_from_start_tests {
    use super::*;
    use crate::core::iso::directory_table::DirectoryEntryAttributes;
    use crate::core::iso::{DirectoryEntry, DirectoryTable};
    use std::io::Cursor;

    const FILE_SECTOR: u32 = 5;
    const ROOT_TABLE_SECTOR: u32 = 200;

    fn root_far_from_start_table() -> DirectoryTable {
        let xbe_entry = DirectoryEntry {
            attributes: DirectoryEntryAttributes::ARCHIVE,
            path: "default.xbe".to_string(),
            name: "default.xbe".to_string(),
            sector: FILE_SECTOR,
            size: SECTOR_SIZE as u32,
        };

        DirectoryTable {
            root_sector: ROOT_TABLE_SECTOR,
            root_size: SECTOR_SIZE as u32,
            entries: vec![xbe_entry],
        }
    }

    #[test]
    fn scan_marks_file_and_root_table_regardless_of_position() {
        let table = root_far_from_start_table();
        let root_offset = 0u64;
        let file_size = (ROOT_TABLE_SECTOR as u64 + 1) * SECTOR_SIZE;

        let info = scan(&table, root_offset, file_size).unwrap();

        assert!(info.used_sectors.contains(&(FILE_SECTOR as u64)));
        assert!(info.used_sectors.contains(&(ROOT_TABLE_SECTOR as u64)));
        assert!(info.used_sectors.contains(&VOLUME_DESCRIPTOR_SECTOR));
        assert_eq!(info.platform, Platform::Ogx);

        assert!(!info.used_sectors.contains(&1));
        assert!(!info.used_sectors.contains(&(FILE_SECTOR as u64 + 1)));
    }

    #[test]
    fn partial_scrub_zeroes_padding_before_and_around_far_root_table() {
        let table = root_far_from_start_table();
        let root_offset = 0u64;
        let file_size = (ROOT_TABLE_SECTOR as u64 + 1) * SECTOR_SIZE;

        let mut reader = Cursor::new(vec![0u8; file_size as usize]);

        let (total_sectors, zero_sectors) = plan_direct(
            ScrubMode::Partial,
            &table,
            root_offset,
            file_size,
            &mut reader,
        )
        .unwrap();

        let zero_sectors = zero_sectors.expect("OGX partial scrub always returns a zero mask");

        assert_eq!(total_sectors, ROOT_TABLE_SECTOR as u64 + 1);

        // The whole leading gap must be scheduled for zeroing.
        for s in 0..FILE_SECTOR as u64 {
            if s != VOLUME_DESCRIPTOR_SECTOR {
                assert!(
                    zero_sectors.contains(&s),
                    "leading padding sector {s} should be zeroed"
                );
            }
        }
        for s in (FILE_SECTOR as u64 + 1)..ROOT_TABLE_SECTOR as u64 {
            if s != VOLUME_DESCRIPTOR_SECTOR {
                assert!(
                    zero_sectors.contains(&s),
                    "gap sector {s} before the root table should be zeroed"
                );
            }
        }

        // Real data must never be in the zero mask.
        assert!(!zero_sectors.contains(&(FILE_SECTOR as u64)));
        assert!(!zero_sectors.contains(&(ROOT_TABLE_SECTOR as u64)));
        assert!(!zero_sectors.contains(&VOLUME_DESCRIPTOR_SECTOR));
    }
}
