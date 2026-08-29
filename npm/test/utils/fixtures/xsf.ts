/**
 * Generates a minimal valid Xbox ISO fixture for testing.
 *
 * Format: XSF containing a single default.xbe (or, for `platform: 'x360'`,
 * default.xex), optionally preceded by `rootOffset` bytes of padding
 * before the XDVDFS volume. Total size: 70 KB of volume content, plus
 * `rootOffset` bytes of leading padding, plus two sectors if
 * `includeSystemUpdate` is set, plus one sector if `largeFileSize` is set
 * (real prefix of a file whose *declared* size can be arbitrarily large).
 *
 * Layout (sector numbers below are relative to `rootOffset`, matching how
 * XDVDFS stores sector numbers relative to its own partition):
 *   Sector 0x20 (0x10000): XDVDFS volume descriptor
 *   Sector 0x21 (0x10800): root directory table
 *     - entry: default.xbe / default.xex               (always present)
 *     - entry: $SystemUpdate  (DIRECTORY)                (opt-in)
 *     - entry: <largeFileName>                           (opt-in)
 *   Sector 0x22 (0x11000): stub executable (XBE or XEX2, see below)
 *   Sector 0x23 (0x11800): $SystemUpdate directory table (opt-in)
 *     - entry: a single dummy file inside $SystemUpdate
 *   Sector 0x24 (0x12000): dummy $SystemUpdate file content (opt-in)
 *   Next free sector: real prefix of the large file        (opt-in)
 *
 * Directory entries form a binary search tree per the XDVDFS spec: the
 * reader starts at DWORD-offset 0 and only visits further entries via a
 * node's `left`/`right` pointers (`0` = no child). Entries here are
 * chained into a minimal right-only tree in write order - unbalanced,
 * but still structurally valid.
 *
 * XBE stub layout (offsets relative to sector 0x22), all fields
 * little-endian:
 *   0x000: magic "XBEH"
 *   0x004: digital signature (256 zero bytes - not validated)
 *   0x104: dw_base_addr      (u32 LE)
 *   0x108: 16 zero bytes
 *   0x118: dw_certificate_addr (u32 LE) = base_addr + 0x200
 *   0x200: certificate
 *     +0x00 [8]:   size + time_date (not read by the parser)
 *     +0x08 [4]:   title_id  (u32 LE)
 *     +0x0c [160]: padding   (not read by the parser)
 *     +0xac [4]:   version   (u32 LE)
 *   Certificate field layout: https://xboxdevwiki.net/Xbe#Certificate
 *
 * XEX2 stub layout (offsets relative to sector 0x22), all fields big-endian:
 *   0x00: magic "XEX2"
 *   0x04: module_flags        (u32 BE) - unused by the parser
 *   0x08: code_offset         (u32 BE) - unused by the parser
 *   0x0c: reserved            (u32 BE) - read and discarded
 *   0x10: certificate_offset  (u32 BE) - unused by the parser
 *   0x14: field_count         (u32 BE) = 1 (just the ExecutionId field)
 *   0x18: field[0].key   (u32 BE) = 0x00040006 (ExecutionId)
 *   0x1c: field[0].value (u32 BE) = XEX_EXECUTION_INFO_OFFSET
 *   0x30 (XEX_EXECUTION_INFO_OFFSET): TitleExecutionInfo, 20 bytes packed:
 *     +0x00 [4]: media_id       (u32 BE) - unused by inspectSource
 *     +0x04 [4]: version        (u32 BE)
 *     +0x08 [4]: base_version   (u32 BE) - unused by inspectSource
 *     +0x0c [4]: title_id       (u32 BE)
 *     +0x10 [1]: platform       (u8)    - unused by inspectSource
 *     +0x11 [1]: executable_type(u8)    - unused by inspectSource
 *     +0x12 [1]: disc_number    (u8)    = 1
 *     +0x13 [1]: disc_count     (u8)    = 1
 *   XEX header field layout: https://free60.org/System-Software/Formats/XEX/
 */
import { writeAscii } from './binary-utils.js';
import {
	THUMBNAIL_XBE_DECLARED_SIZE,
	XEX_THUMBNAIL_EXECUTION_INFO_OFFSET,
	writeXbeThumbnailSection,
	writeXexThumbnailResource,
} from './thumbnail.js';
import type { ThumbnailFixtureOptions } from './thumbnail.js';

export type { ThumbnailFixtureOptions };

const SECTOR_SIZE = 0x800;
const DIR_SECTOR = 0x21;
const XBE_SECTOR = 0x22;
const SYSTEM_UPDATE_DIR_SECTOR = 0x23;
const SYSTEM_UPDATE_FILE_SECTOR = 0x24;

const XBE_BASE_ADDR = 0x10000;
const XBE_CERT_OFFSET = 0x200; // within XBE data
const XBE_CERT_VIRT = XBE_BASE_ADDR + XBE_CERT_OFFSET;
const XBE_CERT_TITLE_ID_OFFSET = 0x08;
// dw_version field offset: https://xboxdevwiki.net/Xbe#Certificate
const XBE_CERT_VERSION_OFFSET = 0xac;

// "Execution ID" optional-header field id:
// https://free60.org/System-Software/Formats/XEX/#header-ids
const XEX_FIELD_ID_EXECUTION_ID = 0x00040006;
const XEX_EXECUTION_INFO_OFFSET = 0x30;

const ATTR_ARCHIVE = 0x20;
const ATTR_DIRECTORY = 0x10;

const SYSTEM_UPDATE_DIR_NAME = '$SystemUpdate';
export const SYSTEM_UPDATE_FILE_NAME = 'su20076000.000';
export const SYSTEM_UPDATE_FILE_SIZE = 0x100;
const MAX_DIRENT_SIZE = 0xffffffff; // DirectoryEntry.size is a u32 LE
const LARGE_FILE_FILL_BYTE = 0xcd;
const DEFAULT_LARGE_FILE_NAME = 'data.bin';
const DEFAULT_LARGE_FILE_PREFIX_SIZE = SECTOR_SIZE;
export const DEFAULT_XBE_DECLARED_SIZE = 0x400;

export interface FixtureOptions {
	titleId?: number;
	/**
	 * XBE version field (platform: 'ogx' only - see `baseVersion` for the
	 * x360 equivalent).
	 */
	version?: number;
	/**
	 * XEX base_version field (platform: 'x360' only). Defaults to 0
	 * ("not a patch") - TitleInfo::version_string omits the "(base ...)"
	 * suffix in that case.
	 */
	baseVersion?: number;
	/**
	 * Bytes of padding to prepend before the XDVDFS volume. Must be a
	 * multiple of SECTOR_SIZE (0x800). Detection only probes four fixed
	 * offsets (Xsf/Xgd2/Xgd1/Xgd3), so other values fail detection despite
	 * being structurally valid. Defaults to 0.
	 */
	rootOffset?: number;
	/**
	 * When true, adds a `$SystemUpdate` directory with one dummy file, so
	 * fixtures can exercise `skipSystemUpdate` end-to-end. Defaults to
	 * false.
	 */
	includeSystemUpdate?: boolean;
	/**
	 * Overrides the on-disk directory entry name written for the system
	 * update directory when `includeSystemUpdate` is true. Defaults to
	 * `$SystemUpdate`. Lets tests write a differently-cased name (e.g.
	 * `$SYSTEMUPDATE`) while keeping every other fixture byte identical, to
	 * confirm `skipSystemUpdate` matches case-insensitively.
	 */
	systemUpdateDirName?: string;
	/**
	 * Which platform's launch executable the fixture builds: controls the
	 * directory entry name and the stub bytes ('ogx' writes XBE, 'x360'
	 * writes XEX2) - detection parses the magic bytes, not the filename.
	 * Defaults to 'ogx'.
	 */
	platform?: 'ogx' | 'x360';
	/**
	 * Overrides the on-disk directory entry name written for the launch
	 * executable. Defaults to `default.xbe` (platform: 'ogx') or
	 * `default.xex` (platform: 'x360'). Lets tests write a differently-cased
	 * or otherwise-varied name (e.g. `DEFAULT.XBE`) while keeping every
	 * other fixture byte - including the stub's magic bytes, which drive
	 * actual detection - identical, to confirm launch-executable lookup
	 * matches case-insensitively.
	 */
	exeName?: string;
	/**
	 * Declared directory-entry size, in bytes, for the launch executable.
	 * Defaults to `DEFAULT_XBE_DECLARED_SIZE` (0x400) - large enough for a
	 * full XBE certificate. Pass a smaller value (e.g. `0x300`) to
	 * exercise the "certificate is out of bounds" error path.
	 *
	 * Only widens the *declared* size - the physical sector backing the
	 * executable is always a full 0x800 bytes.
	 */
	xbeDeclaredSize?: number;
	/**
	 * When set, adds a third root-directory entry (name controlled by
	 * `largeFileName`) whose *declared* size is `largeFileSize` bytes,
	 * independent of how many bytes are physically backed in the
	 * returned buffer.
	 *
	 * Full-rebuild conversion copies `min(declared size, requested size)`
	 * bytes per file, blind to anything past the declared extent -
	 * `largeFileSize` exercises that path without needing that many
	 * bytes physically present.
	 *
	 * Only the first `largeFilePrefixSize` bytes are physically written
	 * (filled with `LARGE_FILE_FILL_BYTE`, 0xcd); bytes beyond the
	 * returned buffer are the caller's responsibility to serve from its
	 * `readFn` (see `makeSparseReadFn`).
	 *
	 * Must fit in a u32 (~4 GiB). Defaults to disabled.
	 */
	largeFileSize?: number;
	/**
	 * Size, in bytes, of the real prefix physically written for the large
	 * file. Must be <= `largeFileSize`. Only used when `largeFileSize` is
	 * set. Defaults to one sector (0x800).
	 */
	largeFilePrefixSize?: number;
	/**
	 * Directory entry name for the large file. Only used when
	 * `largeFileSize` is set. Defaults to "data.bin".
	 */
	largeFileName?: string;
	/**
	 * Embeds a real, decodable launch icon: an XPR0/DXT1 section for
	 * `platform: 'ogx'`, or an XDBF resource-table entry for
	 * `platform: 'x360'`. See `thumbnail.ts` for the layout each parses.
	 *
	 * For `platform: 'ogx'`, widens the effective `xbeDeclaredSize`
	 * default to `THUMBNAIL_XBE_DECLARED_SIZE` unless `xbeDeclaredSize` is
	 * passed explicitly. Not needed for `platform: 'x360'` - the XEX/XDBF
	 * layout fits under the plain default.
	 *
	 * Defaults to disabled.
	 */
	thumbnail?: ThumbnailFixtureOptions;
}

/**
 * Shared sector-layout derivation used by both `makeFixture` and
 * `largeFileByteOffset`, so there is exactly one place this logic lives.
 * Only reads the two options that affect sector placement - safe to call
 * with a partial `FixtureOptions`.
 */
function computeSectorLayout(
	opts: Pick<FixtureOptions, 'includeSystemUpdate' | 'largeFileSize'>,
): { fixedLastSector: number; largeFileSector: number | undefined } {
	const includeSystemUpdate = opts.includeSystemUpdate ?? false;
	const fixedLastSector = includeSystemUpdate
		? SYSTEM_UPDATE_FILE_SECTOR
		: XBE_SECTOR;
	const largeFileSector =
		opts.largeFileSize !== undefined ? fixedLastSector + 1 : undefined;
	return { fixedLastSector, largeFileSector };
}

/**
 * Returns the byte offset at which `makeFixture`'s large-file content
 * starts, for the same `opts` - or `undefined` if `opts.largeFileSize`
 * isn't set. Exists so tests don't have to duplicate `makeFixture`'s
 * internal sector-layout logic. Pass the *same* options object used for
 * the corresponding `makeFixture(opts)` call so the two can't drift.
 */
export function largeFileByteOffset(
	opts: FixtureOptions = {},
): number | undefined {
	const { largeFileSector } = computeSectorLayout(opts);
	return largeFileSector !== undefined
		? largeFileSector * SECTOR_SIZE
		: undefined;
}

export function makeFixture(opts: FixtureOptions = {}): Uint8Array {
	const titleId = opts.titleId ?? 0x41560001;
	const version = opts.version ?? 0x00000001;
	const baseVersion = opts.baseVersion ?? 0;
	const rootOffset = opts.rootOffset ?? 0;
	const includeSystemUpdate = opts.includeSystemUpdate ?? false;
	const systemUpdateDirName = opts.systemUpdateDirName ?? SYSTEM_UPDATE_DIR_NAME;
	const platform = opts.platform ?? 'ogx';
	const thumbnail = opts.thumbnail;
	const xbeDeclaredSize =
		opts.xbeDeclaredSize ??
		(thumbnail && platform !== 'x360'
			? THUMBNAIL_XBE_DECLARED_SIZE
			: DEFAULT_XBE_DECLARED_SIZE);
	const exeName =
		opts.exeName ?? (platform === 'x360' ? 'default.xex' : 'default.xbe');
	const largeFileSize = opts.largeFileSize;
	const largeFileName = opts.largeFileName ?? DEFAULT_LARGE_FILE_NAME;
	const largeFilePrefixSize =
		opts.largeFilePrefixSize ?? DEFAULT_LARGE_FILE_PREFIX_SIZE;

	if (xbeDeclaredSize <= 0 || xbeDeclaredSize > SECTOR_SIZE) {
		throw new Error(
			`xbeDeclaredSize must be > 0 and <= SECTOR_SIZE (${SECTOR_SIZE}), got ${xbeDeclaredSize}`,
		);
	}
	if (rootOffset % SECTOR_SIZE !== 0) {
		throw new Error(
			`rootOffset must be a multiple of SECTOR_SIZE (${SECTOR_SIZE}), got ${rootOffset}`,
		);
	}
	if (largeFileSize !== undefined) {
		if (largeFileSize <= 0 || largeFileSize > MAX_DIRENT_SIZE) {
			throw new Error(
				`largeFileSize must be > 0 and fit in a u32 (<= ${MAX_DIRENT_SIZE}), got ${largeFileSize}`,
			);
		}
		if (largeFilePrefixSize <= 0 || largeFilePrefixSize > largeFileSize) {
			throw new Error(
				`largeFilePrefixSize must be > 0 and <= largeFileSize, got ${largeFilePrefixSize} (largeFileSize=${largeFileSize})`,
			);
		}
	}

	const { fixedLastSector, largeFileSector } = computeSectorLayout({
		includeSystemUpdate,
		largeFileSize,
	});
	const lastSector = largeFileSector ?? fixedLastSector;
	const volumeSize = (lastSector + 1) * SECTOR_SIZE;
	const buf = new Uint8Array(rootOffset + volumeSize);
	const view = new DataView(buf.buffer);

	// Volume descriptor at sector 0x20.
	const VD = rootOffset + 0x20 * SECTOR_SIZE;
	writeAscii(buf, VD + 0x00, 'MICROSOFT*XBOX*MEDIA');
	view.setUint32(VD + 0x14, DIR_SECTOR, true); // root_directory_sector
	view.setUint32(VD + 0x18, 64, true); // root_directory_size
	writeAscii(buf, VD + 0x7ec, 'MICROSOFT*XBOX*MEDIA');

	// Root directory table at sector 0x21.
	// DirectoryEntry binary layout:
	//   +0  u16 LE subtree_left   (DWORD offset from table start; 0 = no child)
	//   +2  u16 LE subtree_right  (DWORD offset from table start; 0 = no child)
	//   +4  u32 LE sector
	//   +8  u32 LE size
	//  +12  u8  attributes        (0x20 = ARCHIVE, 0x10 = DIRECTORY)
	//  +13  u8  name_length
	//  +14  u8[] name (ASCII)
	//  then pad to 4-byte alignment
	const DIR = rootOffset + DIR_SECTOR * SECTOR_SIZE;
	const rootEntryOffsets: number[] = [];
	let cursor = DIR;

	rootEntryOffsets.push(cursor);
	cursor = writeDirectoryEntry(buf, view, cursor, {
		name: exeName,
		sector: XBE_SECTOR,
		size: xbeDeclaredSize,
		attributes: ATTR_ARCHIVE,
	});

	if (includeSystemUpdate) {
		rootEntryOffsets.push(cursor);
		cursor = writeDirectoryEntry(buf, view, cursor, {
			name: systemUpdateDirName,
			sector: SYSTEM_UPDATE_DIR_SECTOR,
			size: SECTOR_SIZE, // one sector holds the subdirectory's table
			attributes: ATTR_DIRECTORY,
		});
	}

	if (largeFileSize !== undefined && largeFileSector !== undefined) {
		rootEntryOffsets.push(cursor);
		cursor = writeDirectoryEntry(buf, view, cursor, {
			name: largeFileName,
			sector: largeFileSector,
			size: largeFileSize,
			attributes: ATTR_ARCHIVE,
		});
	}

	// Chain entries into a minimal right-only tree: each entry's `right`
	// points at the DWORD offset (from `DIR`) of the next entry written.
	// `left` stays 0 on every entry.
	for (let i = 0; i < rootEntryOffsets.length - 1; i++) {
		const rightDwordOffset = (rootEntryOffsets[i + 1] - DIR) / 4;
		view.setUint16(rootEntryOffsets[i] + 2, rightDwordOffset, true); // right
	}

	if (includeSystemUpdate) {
		// $SystemUpdate directory table at sector 0x23.
		const SUB_DIR = rootOffset + SYSTEM_UPDATE_DIR_SECTOR * SECTOR_SIZE;
		writeDirectoryEntry(buf, view, SUB_DIR, {
			name: SYSTEM_UPDATE_FILE_NAME,
			sector: SYSTEM_UPDATE_FILE_SECTOR,
			size: SYSTEM_UPDATE_FILE_SIZE,
			attributes: ATTR_ARCHIVE,
		});

		// Dummy $SystemUpdate file content at sector 0x24.
		const SU_FILE = rootOffset + SYSTEM_UPDATE_FILE_SECTOR * SECTOR_SIZE;
		buf.fill(0xaa, SU_FILE, SU_FILE + SYSTEM_UPDATE_FILE_SIZE);
	}

	if (largeFileSize !== undefined && largeFileSector !== undefined) {
		// Real prefix of the large file.
		const LARGE_FILE = rootOffset + largeFileSector * SECTOR_SIZE;
		buf.fill(LARGE_FILE_FILL_BYTE, LARGE_FILE, LARGE_FILE + largeFilePrefixSize);
	}

	// Stub executable at sector 0x22.
	const EXE = rootOffset + XBE_SECTOR * SECTOR_SIZE;
	if (platform === 'x360') {
		const executionInfoOffset = thumbnail
			? XEX_THUMBNAIL_EXECUTION_INFO_OFFSET
			: XEX_EXECUTION_INFO_OFFSET;
		writeXexStub(
			buf,
			view,
			EXE,
			{ titleId, version, baseVersion },
			executionInfoOffset,
		);
		if (thumbnail) {
			writeXexThumbnailResource(buf, view, EXE);
		}
	} else {
		writeXbeStub(buf, view, EXE, { titleId, version });
		if (thumbnail) {
			writeXbeThumbnailSection(
				buf,
				view,
				EXE,
				XBE_BASE_ADDR,
				thumbnail.xbeSectionName ?? '$$XTIMAGE',
			);
		}
	}

	return buf;
}

/** Writes a minimal XBE-shaped stub (see module doc comment). All
 * multi-byte fields little-endian. */
function writeXbeStub(
	buf: Uint8Array,
	view: DataView,
	offset: number,
	info: { titleId: number; version: number },
): void {
	writeAscii(buf, offset + 0x000, 'XBEH');
	view.setUint32(offset + 0x104, XBE_BASE_ADDR, true);
	view.setUint32(offset + 0x118, XBE_CERT_VIRT, true);
	const CERT = offset + XBE_CERT_OFFSET;
	view.setUint32(CERT + XBE_CERT_TITLE_ID_OFFSET, info.titleId, true);
	view.setUint32(CERT + XBE_CERT_VERSION_OFFSET, info.version, true);
}

/** Writes a minimal XEX2-shaped stub (see module doc comment). All
 * multi-byte fields big-endian. Only the one optional-header field the
 * parser actually acts on (`ExecutionId`) is included; every other field
 * it reads along the way is left zeroed. */
function writeXexStub(
	buf: Uint8Array,
	view: DataView,
	offset: number,
	info: { titleId: number; version: number; baseVersion?: number },
	executionInfoOffset: number = XEX_EXECUTION_INFO_OFFSET,
): void {
	writeAscii(buf, offset + 0x00, 'XEX2');
	view.setUint32(offset + 0x14, 1, false); // field_count
	view.setUint32(offset + 0x18, XEX_FIELD_ID_EXECUTION_ID, false);
	view.setUint32(offset + 0x1c, executionInfoOffset, false);

	const INFO = offset + executionInfoOffset;
	view.setUint32(INFO + 0x00, 0, false); // media_id
	view.setUint32(INFO + 0x04, info.version, false);
	view.setUint32(INFO + 0x08, info.baseVersion ?? 0, false);
	view.setUint32(INFO + 0x0c, info.titleId, false);
	buf[INFO + 0x10] = 0; // platform
	buf[INFO + 0x11] = 0; // executable_type
	buf[INFO + 0x12] = 1; // disc_number
	buf[INFO + 0x13] = 1; // disc_count
}

/**
 * Writes a single DirectoryEntry at `offset` and returns the offset of the
 * next entry (4-byte aligned). `subtree_left`/`subtree_right` default to 0
 * ("no child") - the caller wires up `right` pointers afterward when more
 * than one entry ends up in the same table.
 */
function writeDirectoryEntry(
	buf: Uint8Array,
	view: DataView,
	offset: number,
	entry: { name: string; sector: number; size: number; attributes: number },
): number {
	view.setUint16(offset + 0, 0x0000, true); // subtree_left
	view.setUint16(offset + 2, 0x0000, true); // subtree_right
	view.setUint32(offset + 4, entry.sector, true);
	view.setUint32(offset + 8, entry.size, true);
	buf[offset + 12] = entry.attributes;
	buf[offset + 13] = entry.name.length;
	writeAscii(buf, offset + 14, entry.name);
	const used = 14 + entry.name.length;
	const aligned = Math.ceil(used / 4) * 4;
	return offset + aligned;
}
