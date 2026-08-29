// STFS (LIVE/PIRS/CON) fixture builders for tests.
// Layout reference: https://free60.org/System-Software/Formats/STFS/
import {
	writeXexThumbnailResource,
	XEX_THUMBNAIL_EXECUTION_INFO_OFFSET,
} from './thumbnail.js';

// "Content Packages" header table:
// https://free60.org/System-Software/Formats/STFS/#content-packages
export const HEADER_SIZE_FIELD_OFFSET = 0x340;
export const CONTENT_TYPE_FIELD_OFFSET = 0x344;
export const VOLUME_DESCRIPTOR_OFFSET = 0x379;

/** Display Name field (UTF-16BE, fixed-width, up to 0x80 code units) -
 * read by `StfsReader::open` (`format::read_display_name`) and written
 * by the write session's `build_header` (`header_offset::DISPLAY_NAME`).
 * Same offset both directions. */
export const DISPLAY_NAME_FIELD_OFFSET = 0x411;
export const DISPLAY_NAME_FIELD_MAX_UNITS = 0x80;

/** AvatarItem-only structured metadata region (subCategory u32 LE,
 * colorizable u32 LE, 16-byte guid, skeletonVersion u8 - 25 bytes
 * total). Only read/written when `content_type == AvatarItem`.
 * Little-endian despite sitting in an otherwise big-endian header,
 * matching Velocity's explicit SwapEndian() around this region. */
export const AVATAR_ITEM_METADATA_FIELD_OFFSET = 0x3d9;
export const AVATAR_ITEM_METADATA_FIELD_LEN = 25;

/** "Descriptor type" (Velocity's `fileSystem` discriminant) - a
 * big-endian u32, not a single byte. `0` means the volume descriptor
 * at `VOLUME_DESCRIPTOR_OFFSET` is STFS-shaped, which
 * `StfsReader::open` requires; every fixture defaults to this. */
export const DESCRIPTOR_TYPE_FIELD_OFFSET = 0x3a9;

// Header-embedded image fields, shared layout with GoD headers - see
// `format::header_offset::{THUMBNAIL_SIZE, TITLE_THUMBNAIL_SIZE,
// THUMBNAIL_IMAGE, TITLE_THUMBNAIL_IMAGE}` on the Rust side.
export const THUMBNAIL_SIZE_FIELD_OFFSET = 0x1712;
export const TITLE_THUMBNAIL_SIZE_FIELD_OFFSET = 0x1716;
export const THUMBNAIL_IMAGE_OFFSET = 0x171a;
export const TITLE_THUMBNAIL_IMAGE_OFFSET = 0x571a;
/** Metadata Version field (BE u32) - `2` shrinks the max accepted header
 * image size. Written by `makeStfsFixture` only when `metadataVersion` is
 * explicitly passed; every fixture otherwise leaves it unwritten (i.e. 0,
 * the V1 default). */
export const METADATA_VERSION_FIELD_OFFSET = 0x348;

/**
 * Max accepted byte size for a header-embedded Thumbnail/Title Thumbnail
 * Image, at Metadata Version 1 (the default) vs Version 2. Mirrors
 * `THUMBNAIL_MAX_SIZE_V1`/`THUMBNAIL_MAX_SIZE_V2` in `format.rs` (private
 * there, so kept in sync by hand).
 */
export const HEADER_THUMBNAIL_MAX_SIZE_V1 = 0x4000;
export const HEADER_THUMBNAIL_MAX_SIZE_V2 = 0x3d00;

// Hash-tree record layout, "Hash Tables / Block Offsets":
// https://free60.org/System-Software/Formats/STFS/#hash-tables-block-offsets
const HASH_ENTRY_SIZE = 0x18; // 0x14-byte hash + 1-byte status + 3-byte next-block
const STATUS_BYTE_OFFSET = 0x14;
const NEXT_BLOCK_OFFSET = 0x15;
const CHAIN_TERMINATOR = 0xffffff;
export const BLOCK_SIZE = 0x1000;
export const ROOT_PATH_INDICATOR = 0xffff;

// A level-0 hash table covers 0xAA (170) blocks; a level-1 table covers
// 0xAA level-0 tables' worth (0xAA^2 = 0x70E4).
const LEVEL0_GROUP_SIZE = 0xaa;
const LEVEL1_GROUP_SIZE = 0x70e4; // == LEVEL0_GROUP_SIZE ** 2

// blockStep pair for blockSeparation = 1 (shift = 0, no spacer block) -
// the only case used by fixtures in this file.
// https://free60.org/System-Software/Formats/STFS/#file-listing
const BLOCK_STEP_SHIFT0: readonly [number, number] = [0xab, 0x718f];

// Optional-header entry for MediaID/Version/BaseVersion/TitleID:
// https://free60.org/System-Software/Formats/XEX/
const XEX_FIELD_ID_EXECUTION_ID = 0x00040006;
const XEX_EXECUTION_INFO_OFFSET = 0x30;

function writeAscii(buf: Uint8Array, offset: number, s: string): void {
	for (let i = 0; i < s.length; i++) {
		buf[offset + i] = s.charCodeAt(i);
	}
}

/** Writes `s` as fixed-width UTF-16BE, truncated to `maxUnits` code
 * units - mirrors `build_header`'s DISPLAY_NAME encoding on the write
 * side (`write_meta_offset::DISPLAY_NAME_MAX_UNITS` == 0x80). Only
 * handles the BMP subset `s.charCodeAt` returns, matching every caller
 * here (ASCII test names). */
function writeUtf16Be(
	view: DataView,
	offset: number,
	s: string,
	maxUnits: number,
): void {
	const units = Math.min(s.length, maxUnits);
	for (let i = 0; i < units; i++) {
		view.setUint16(offset + i * 2, s.charCodeAt(i), false);
	}
}

function writeInt24LE(view: DataView, offset: number, value: number): void {
	view.setUint8(offset, value & 0xff);
	view.setUint8(offset + 1, (value >> 8) & 0xff);
	view.setUint8(offset + 2, (value >> 16) & 0xff);
}

function writeInt24BE(view: DataView, offset: number, value: number): void {
	view.setUint8(offset, (value >> 16) & 0xff);
	view.setUint8(offset + 1, (value >> 8) & 0xff);
	view.setUint8(offset + 2, value & 0xff);
}

// Matches Velocity/XboxInternals's StfsPackage::ComputeBackingDataBlockNumber
// term for term. Deliberately does not match free60.org's own C# sample,
// which gates the shift on Magic == CON and overrides it based on
// header_size - free60 itself warns that sample "may not work perfectly".
function computeBackingDataBlockNumber(n: number, shift: number): number {
	const base =
		(Math.floor((n + LEVEL0_GROUP_SIZE) / LEVEL0_GROUP_SIZE) << shift) + n;
	if (n < LEVEL0_GROUP_SIZE) return base;
	if (n < LEVEL1_GROUP_SIZE) {
		return (
			base + (Math.floor((n + LEVEL1_GROUP_SIZE) / LEVEL1_GROUP_SIZE) << shift)
		);
	}
	return (
		(1 << shift) +
		base +
		(Math.floor((n + LEVEL1_GROUP_SIZE) / LEVEL1_GROUP_SIZE) << shift)
	);
}

function computeLevel0BackingHashBlockNumber(
	n: number,
	shift: number,
	blockStep0: number,
): number {
	if (n < LEVEL0_GROUP_SIZE) return 0;
	let num = Math.floor(n / LEVEL0_GROUP_SIZE) * blockStep0;
	num += (Math.floor(n / LEVEL1_GROUP_SIZE) + 1) << shift;
	if (Math.floor(n / LEVEL1_GROUP_SIZE) === 0) return num;
	return num + (1 << shift);
}

function computeLevel1BackingHashBlockNumber(
	n: number,
	shift: number,
	blockStep0: number,
	blockStep1: number,
): number {
	if (n < LEVEL1_GROUP_SIZE) return blockStep0;
	return (1 << shift) + Math.floor(n / LEVEL1_GROUP_SIZE) * blockStep1;
}

/** Backing-block number -> absolute file offset. Used for both data
 * blocks and hash-table blocks. */
function blockToFileAddress(
	backingBlock: number,
	firstHashTableAddress: number,
): number {
	return (backingBlock << 0xc) + firstHashTableAddress;
}

/** Writes one hash-tree record's status+next-block fields (allocated,
 * chain-terminated) at `localOffset`. The hash itself is left zeroed -
 * the reader never verifies it. */
function writeHashEntry(
	buf: Uint8Array,
	view: DataView,
	localOffset: number,
): void {
	buf[localOffset + STATUS_BYTE_OFFSET] = 0x80; // status: allocated
	writeInt24BE(view, localOffset + NEXT_BLOCK_OFFSET, CHAIN_TERMINATOR);
}

/** Same field layout as xsf.ts's XEX2 stub writer, all fields big-endian.
 * `executionInfoOffset` defaults to the no-thumbnail offset; pass
 * `XEX_THUMBNAIL_EXECUTION_INFO_OFFSET` when embedding a thumbnail. */
function writeXexStub(
	buf: Uint8Array,
	view: DataView,
	offset: number,
	info: { titleId: number; version: number },
	executionInfoOffset: number = XEX_EXECUTION_INFO_OFFSET,
): void {
	writeAscii(buf, offset + 0x00, 'XEX2');
	view.setUint32(offset + 0x14, 1, false); // field_count
	view.setUint32(offset + 0x18, XEX_FIELD_ID_EXECUTION_ID, false);
	view.setUint32(offset + 0x1c, executionInfoOffset, false);
	const INFO = offset + executionInfoOffset;
	view.setUint32(INFO + 0x00, 0, false); // media_id
	view.setUint32(INFO + 0x04, info.version, false);
	view.setUint32(INFO + 0x08, 0, false); // base_version
	view.setUint32(INFO + 0x0c, info.titleId, false);
	buf[INFO + 0x10] = 0; // platform
	buf[INFO + 0x11] = 0; // executable_type
	buf[INFO + 0x12] = 1; // disc_number
	buf[INFO + 0x13] = 1; // disc_count
}

/**
 * Generates a minimal, structurally valid STFS (LIVE/PIRS/CON) package.
 *
 * Level-zero only: always uses `allocatedBlockCount = 2` (one file-table
 * block + one data block). See `makeStfsLevelOneFixture`/
 * `makeStfsLevelTwoFixture` for topLevel One/Two.
 *
 * Physical layout (default `headerSize = 0x1000`, `blockSeparation = 1`,
 * so `firstHashTableAddress = 0x1000`, shift 0, no spacer block):
 *   0x000:          magic (4 bytes: "CON ", "LIVE", or "PIRS")
 *   0x340:          header_size (u32 BE)
 *   0x379..0x39D:   volume descriptor (0x24 bytes)
 *   0x1000..0x2000: the package's only hash table block - entries for
 *                   block 0 (file-table) and block 1 (data)
 *   0x2000..0x3000: file-table block - one entry for `fileName`
 *   0x3000..0x4000: file data - a minimal XEX2 stub, or the resource
 *                   table + XDBF blob from `thumbnail.ts` when
 *                   `thumbnail` is set
 */
export interface StfsFixtureOptions {
	/** Package magic. Defaults to 'CON ' (note the trailing space - all
	 * three magics are exactly 4 bytes). */
	magic?: 'CON ' | 'LIVE' | 'PIRS';
	/** Raw `header_size` metadata field at absolute offset 0x340. Only its
	 * effect on `firstHashTableAddress = (headerSize + 0xFFF) & 0xFFFFF000`
	 * matters here. Defaults to 0x1000. */
	headerSize?: number;
	/** Raw `blockSeparation` byte. Defaults to 1 (shift 0, no spacer
	 * block - the simplest addressing case). This fixture stays at
	 * topLevel == Zero regardless, so blockStep is never read. */
	blockSeparation?: number;
	/** Xbox 360 title ID written into the XEX2 stub. Defaults to
	 * 0x5a5a0001. */
	titleId?: number;
	/** Version field written into the XEX2 stub. Defaults to 1. */
	version?: number;
	/** Name of the package's single file. Defaults to 'default.xex'.
	 * Must be 1-40 ASCII characters (on-disk name field is 0x28 bytes
	 * wide). */
	fileName?: string;
	/** Raw `content_type` metadata field at absolute offset 0x344. Left
	 * unwritten (0) by default. Pass a real discriminant (0x7000
	 * GamesOnDemand / 0x5000 XboxOriginal / 0xD0000 ArcadeGame) to test
	 * resolving a source's own declared content type. */
	contentType?: number;
	/** Embeds a real, decodable launch icon into the XEX2 stub via
	 * `writeXexThumbnailResource` from `thumbnail.ts`. Defaults to
	 * disabled. */
	thumbnail?: Record<string, never>;
	/** Raw bytes to write into the file's data block instead of the
	 * default XEX2 stub - lets a fixture carry something other than a
	 * launch executable. When set, the declared file size reflects
	 * `fileBytes.length` and `thumbnail`/`titleId`/`version` are ignored.
	 * Must fit in one data block (<= 0x1000 bytes). */
	fileBytes?: Uint8Array;
	/**
	 * Raw bytes for the header's Thumbnail Image field (0x171A, size at
	 * 0x1712) - see `makeHeaderThumbnailBytes` in `thumbnail.ts` for bytes
	 * that pass the reader's PNG-magic check. Independent of `thumbnail`
	 * (which embeds an icon in the XEX2 stub instead). Setting this or
	 * `headerTitleThumbnail` bumps the default `headerSize` so the image
	 * fields don't collide with the blocks that follow; pass `headerSize`
	 * explicitly to override.
	 */
	headerThumbnail?: Uint8Array;
	/** Same as `headerThumbnail`, for the Title Thumbnail Image field
	 * (0x571A, size at 0x1716). */
	headerTitleThumbnail?: Uint8Array;
	/**
	 * Raw `Metadata Version` field (BE u32) at absolute offset 0x348.
	 * Left unwritten (0, i.e. Version 1) by default. Pass `2` to exercise
	 * the reader's shrunk max header-image size.
	 */
	metadataVersion?: number;
	/**
	 * Raw Display Name field (UTF-16BE, fixed-width, @ 0x411) baked
	 * directly into the package's own header, independent of
	 * `titleId`/game-list lookup. Lets a fixture assert that a
	 * stfs->stfs conversion preserves the source's own display name.
	 * Left unwritten (no display name) by default.
	 */
	displayName?: string;
	/**
	 * AvatarItem-only structured metadata baked into the header at
	 * `AVATAR_ITEM_METADATA_FIELD_OFFSET` (0x3D9). Independent of
	 * `contentType` - callers must also set
	 * `contentType: STFS_CONTENT_TYPE.avatarItem` for the reader to look
	 * at this region. `skeletonVersion` must be 1-3 (Velocity's validated
	 * range) for the fixture to round-trip as present.
	 */
	avatarItemMetadata?: {
		subCategory: number;
		colorizable: number;
		guid: Uint8Array;
		skeletonVersion: number;
	};
}

export interface StfsFixtureResult {
	/** The raw package bytes - pass to makeReadFn/inspectSource. */
	bytes: Uint8Array;
	/** Absolute file offset of file-table block 0 (backing block 0). */
	fileTableAddr: number;
	/** Absolute file offset of the data block (backing block 1) - the
	 * XEX2 stub's location. */
	dataAddr: number;
}

export function makeStfsFixture(
	opts: StfsFixtureOptions = {},
): StfsFixtureResult {
	const magic = opts.magic ?? 'CON ';
	const headerThumbnail = opts.headerThumbnail;
	const headerTitleThumbnail = opts.headerTitleThumbnail;
	// A real header image can run up to 0x4000 bytes each at 0x171A/0x571A,
	// nowhere near fitting under the plain 0x1000 default, so bump the
	// header size out to what a real write session pads to
	// (STFS_WRITE_DEFAULT_HEADER_SIZE, below) whenever either is embedded.
	const headerSize =
		opts.headerSize ??
		(headerThumbnail || headerTitleThumbnail ? 0x971a : 0x1000);
	const blockSeparation = opts.blockSeparation ?? 1;
	const titleId = opts.titleId ?? 0x5a5a0001;
	const version = opts.version ?? 1;
	const fileName = opts.fileName ?? 'default.xex';
	const contentType = opts.contentType ?? 0;
	const metadataVersion = opts.metadataVersion;
	const thumbnail = opts.thumbnail;
	const fileBytes = opts.fileBytes;
	if (fileName.length === 0 || fileName.length > 0x28) {
		throw new Error(
			`fileName must be 1-40 ASCII characters, got length ${fileName.length}`,
		);
	}
	if (fileBytes && fileBytes.length > BLOCK_SIZE) {
		throw new Error(
			`fileBytes must fit in one data block (<= ${BLOCK_SIZE} bytes), got ${fileBytes.length}`,
		);
	}
	const FILE_TABLE_BLOCK = 0;
	const DATA_BLOCK = 1;
	const ALLOCATED_BLOCK_COUNT = 2;
	const shift = ~blockSeparation & 1;
	const firstHashTableAddress = (headerSize + 0xfff) & 0xfffff000;
	for (const [label, bytes, fieldOffset] of [
		['headerThumbnail', headerThumbnail, THUMBNAIL_IMAGE_OFFSET],
		['headerTitleThumbnail', headerTitleThumbnail, TITLE_THUMBNAIL_IMAGE_OFFSET],
	] as const) {
		if (bytes && fieldOffset + bytes.length > firstHashTableAddress) {
			throw new Error(
				`${label} (${bytes.length} bytes at 0x${fieldOffset.toString(16)}) doesn't fit ` +
					`before firstHashTableAddress (0x${firstHashTableAddress.toString(16)}) ` +
					`- pass a larger headerSize`,
			);
		}
	}

	function blockToAddress(blockNum: number): number {
		return blockToFileAddress(
			computeBackingDataBlockNumber(blockNum, shift),
			firstHashTableAddress,
		);
	}
	function hashAddressOfBlock(blockNum: number): number {
		return (
			(computeLevel0BackingHashBlockNumber(
				blockNum,
				shift,
				BLOCK_STEP_SHIFT0[0],
			) <<
				0xc) +
			firstHashTableAddress +
			(blockNum % LEVEL0_GROUP_SIZE) * HASH_ENTRY_SIZE +
			((blockSeparation & 2) << 0xb)
		);
	}

	const fileTableAddr = blockToAddress(FILE_TABLE_BLOCK);
	const dataAddr = blockToAddress(DATA_BLOCK);
	const declaredSize = fileBytes ? fileBytes.length : 0x100;
	const totalSize = dataAddr + 0x1000;
	const buf = new Uint8Array(totalSize);
	const view = new DataView(buf.buffer);

	// header
	writeAscii(buf, 0, magic);
	view.setUint32(HEADER_SIZE_FIELD_OFFSET, headerSize, false);
	view.setUint32(CONTENT_TYPE_FIELD_OFFSET, contentType, false);
	if (metadataVersion !== undefined) {
		view.setUint32(METADATA_VERSION_FIELD_OFFSET, metadataVersion, false);
	}
	if (headerThumbnail) {
		view.setUint32(THUMBNAIL_SIZE_FIELD_OFFSET, headerThumbnail.length, false);
		buf.set(headerThumbnail, THUMBNAIL_IMAGE_OFFSET);
	}
	if (headerTitleThumbnail) {
		view.setUint32(
			TITLE_THUMBNAIL_SIZE_FIELD_OFFSET,
			headerTitleThumbnail.length,
			false,
		);
		buf.set(headerTitleThumbnail, TITLE_THUMBNAIL_IMAGE_OFFSET);
	}
	if (opts.displayName !== undefined) {
		writeUtf16Be(
			view,
			DISPLAY_NAME_FIELD_OFFSET,
			opts.displayName,
			DISPLAY_NAME_FIELD_MAX_UNITS,
		);
	}
	if (opts.avatarItemMetadata !== undefined) {
		const { subCategory, colorizable, guid, skeletonVersion } =
			opts.avatarItemMetadata;
		if (guid.length !== 16) {
			throw new Error(
				`avatarItemMetadata.guid must be 16 bytes, got ${guid.length}`,
			);
		}
		const AIM = AVATAR_ITEM_METADATA_FIELD_OFFSET;
		view.setUint32(AIM, subCategory, true);
		view.setUint32(AIM + 4, colorizable, true);
		buf.set(guid, AIM + 8);
		buf[AIM + 24] = skeletonVersion;
	}

	// volume descriptor @ 0x379
	const VD = VOLUME_DESCRIPTOR_OFFSET;
	buf[VD + 0] = 0x24; // descriptor's own size
	buf[VD + 1] = 0;
	buf[VD + 2] = blockSeparation;
	view.setUint16(VD + 3, 1, true);
	writeInt24LE(view, VD + 5, FILE_TABLE_BLOCK);
	view.setUint32(VD + 0x1c, ALLOCATED_BLOCK_COUNT, false);
	view.setUint32(VD + 0x20, 0, false);

	// the one hash table block, at firstHashTableAddress
	for (const block of [FILE_TABLE_BLOCK, DATA_BLOCK]) {
		writeHashEntry(buf, view, hashAddressOfBlock(block));
	}

	// file-table block (block 0): one entry for fileName
	writeAscii(buf, fileTableAddr, fileName);
	buf[fileTableAddr + 0x28] = fileName.length;
	writeInt24LE(view, fileTableAddr + 0x29, 1);
	writeInt24LE(view, fileTableAddr + 0x2c, 1); // copy of 0x29 (blocksForFile)
	writeInt24LE(view, fileTableAddr + 0x2f, DATA_BLOCK);
	view.setUint16(fileTableAddr + 0x32, 0xffff, false);
	view.setUint32(fileTableAddr + 0x34, declaredSize, false);
	// file data (block 1): caller-supplied bytes, or a minimal XEX2 stub
	if (fileBytes) {
		buf.set(fileBytes, dataAddr);
	} else {
		const executionInfoOffset = thumbnail
			? XEX_THUMBNAIL_EXECUTION_INFO_OFFSET
			: XEX_EXECUTION_INFO_OFFSET;
		writeXexStub(buf, view, dataAddr, { titleId, version }, executionInfoOffset);
		if (thumbnail) {
			writeXexThumbnailResource(buf, view, dataAddr);
		}
	}

	return { bytes: buf, fileTableAddr, dataAddr };
}

/**
 * Generates a minimal, structurally valid **topLevel == One** STFS
 * package - exercises the 2-entry top table, which topLevel == Zero
 * never touches.
 *
 * Forces topLevel == One via allocatedBlockCount = 0xAB (171), one block
 * past the `<= 0xAA` ceiling. The one file sits at logical block 0xAA
 * (170), the first block of the second group, so reading it walks
 * top_table[1], not just top_table[0].
 *
 * blockSeparation is fixed at 1 (shift 0, blockStep[0] = 0xAB).
 *
 * Physical layout (headerSize fixed at 0x1000, firstHashTableAddress =
 * 0x1000):
 *   0x1000..0x2000: backing block 0 - group 0's level-0 hash table, and
 *                   also the 2-entry top table for topLevel == One. Only
 *                   the first two records are written: record 0 is block
 *                   0's hash entry, record 1 is block 1's hash entry /
 *                   top_table[1]
 *   0x2000..0x3000: backing block 1 - file-table block (logical block 0),
 *                   one entry for `fileName`, startingBlockNum = 0xAA
 *   0xAD000..0xAE000: backing block 0xAC - group 1's level-0 hash
 *                   subtable; only record 0 written (block 0xAA's entry)
 *   0xAE000..0xAF000: backing block 0xAD - file data (logical block
 *                   0xAA) - the XEX2 stub
 */
export interface StfsLevelOneFixtureOptions {
	magic?: 'CON ' | 'LIVE' | 'PIRS';
	titleId?: number;
	version?: number;
	fileName?: string;
}

export function makeStfsLevelOneFixture(
	opts: StfsLevelOneFixtureOptions = {},
): Uint8Array {
	const magic = opts.magic ?? 'CON ';
	const headerSize = 0x1000;
	const blockSeparation = 1; // shift 0
	const titleId = opts.titleId ?? 0x5a5a0001;
	const version = opts.version ?? 1;
	const fileName = opts.fileName ?? 'default.xex';

	if (fileName.length === 0 || fileName.length > 0x28) {
		throw new Error(
			`fileName must be 1-40 ASCII characters, got length ${fileName.length}`,
		);
	}

	const ALLOCATED_BLOCK_COUNT = 0xab; // 1 past 0xAA -> forces topLevel == One
	const FILE_TABLE_BLOCK = 0; // logical
	const DATA_BLOCK = LEVEL0_GROUP_SIZE; // logical - first block of group 1
	const [BLOCK_STEP0] = BLOCK_STEP_SHIFT0;
	const shift = ~blockSeparation & 1;
	const firstHashTableAddress = (headerSize + 0xfff) & 0xfffff000;

	const fileTableAddr = blockToFileAddress(
		computeBackingDataBlockNumber(FILE_TABLE_BLOCK, shift),
		firstHashTableAddress,
	);
	const dataAddr = blockToFileAddress(
		computeBackingDataBlockNumber(DATA_BLOCK, shift),
		firstHashTableAddress,
	);
	const group0HashAddr = blockToFileAddress(0, firstHashTableAddress);
	const group1HashAddr = blockToFileAddress(
		computeLevel0BackingHashBlockNumber(DATA_BLOCK, shift, BLOCK_STEP0),
		firstHashTableAddress,
	);

	const xexDeclaredSize = 0x100;
	const totalSize = dataAddr + 0x1000;
	const buf = new Uint8Array(totalSize);
	const view = new DataView(buf.buffer);

	// header
	writeAscii(buf, 0, magic);
	view.setUint32(HEADER_SIZE_FIELD_OFFSET, headerSize, false);

	// volume descriptor @ 0x379
	const VD = VOLUME_DESCRIPTOR_OFFSET;
	buf[VD + 0] = 0x24; // descriptor's own size
	buf[VD + 1] = 0;
	buf[VD + 2] = blockSeparation;
	view.setUint16(VD + 3, 1, true);
	writeInt24LE(view, VD + 5, FILE_TABLE_BLOCK);
	view.setUint32(VD + 0x1c, ALLOCATED_BLOCK_COUNT, false);
	view.setUint32(VD + 0x20, 0, false);

	// group 0's hash table / top table, at backing block 0
	writeHashEntry(buf, view, group0HashAddr); // block 0's hash entry
	// block 1's hash entry, and simultaneously top_table[1] - status bit
	// 0x40 clear here keeps group 1's hash subtable at its plain address.
	writeHashEntry(buf, view, group0HashAddr + HASH_ENTRY_SIZE);

	// file-table block (backing block 1 / logical block 0)
	writeAscii(buf, fileTableAddr, fileName);
	buf[fileTableAddr + 0x28] = fileName.length;
	writeInt24LE(view, fileTableAddr + 0x29, 1);
	writeInt24LE(view, fileTableAddr + 0x2c, 1); // copy of 0x29 (blocksForFile)
	writeInt24LE(view, fileTableAddr + 0x2f, DATA_BLOCK);
	view.setUint16(fileTableAddr + 0x32, 0xffff, false);
	view.setUint32(fileTableAddr + 0x34, xexDeclaredSize, false);

	// group 1's hash subtable, at backing block 0xAC
	writeHashEntry(buf, view, group1HashAddr);

	// file data (backing block 0xAD / logical block 0xAA)
	writeXexStub(buf, view, dataAddr, { titleId, version });

	return buf;
}

/** Backs a sparse STFS fixture: a handful of real 0x1000-byte chunks at
 * arbitrary absolute file offsets, with every other byte read as zero.
 * Used by `makeStfsLevelTwoFixture`, whose logical file size (~114MB) is
 * too large to materialize as a flat buffer. */
class SparseChunks {
	private readonly chunks = new Map<
		number,
		{ buf: Uint8Array; view: DataView }
	>();

	private at(absAddr: number): {
		buf: Uint8Array;
		view: DataView;
		local: number;
	} {
		const base = absAddr - (absAddr % BLOCK_SIZE);
		let chunk = this.chunks.get(base);
		if (!chunk) {
			const buf = new Uint8Array(BLOCK_SIZE);
			chunk = { buf, view: new DataView(buf.buffer) };
			this.chunks.set(base, chunk);
		}
		return { ...chunk, local: absAddr - base };
	}

	writeAt(
		absAddr: number,
		fn: (buf: Uint8Array, view: DataView, local: number) => void,
	): void {
		const { buf, view, local } = this.at(absAddr);
		fn(buf, view, local);
	}

	writeHashEntryAt(absAddr: number): void {
		this.writeAt(absAddr, writeHashEntry);
	}

	toFixture(length: number): SparseStfsFixture {
		const chunks = this.chunks;
		const readFn = (offset: number, len: number): Uint8Array => {
			const out = new Uint8Array(len);
			for (const [chunkOffset, { buf }] of chunks) {
				const start = Math.max(offset, chunkOffset);
				const end = Math.min(offset + len, chunkOffset + buf.length);
				if (start < end) {
					out.set(
						buf.subarray(start - chunkOffset, end - chunkOffset),
						start - offset,
					);
				}
			}
			return out;
		};
		return { length, readFn };
	}
}

/**
 * Generates a minimal, structurally valid **topLevel == Two** STFS
 * package - exercises a live seek+read into an intermediate level-1
 * subtable's status byte, which topLevel Zero/One never touch.
 *
 * Returns a sparse `{ length, readFn }` instead of a flat buffer: forcing
 * topLevel == Two requires allocatedBlockCount > 0x70E4, and the smallest
 * block number in the second top-level group is 0x70E4 itself - at 0x1000
 * bytes/block, a ~114MB logical file even though only a few 4KB regions
 * hold real bytes.
 *
 * allocatedBlockCount is fixed at 0x70E5, one block past the topLevel ==
 * One ceiling. The package's one file sits at logical block 0x70E4 (28900) -
 * the first block of top-level group 1 - so reading it walks
 * top_table[1], not just top_table[0].
 *
 * blockSeparation is fixed at 1 (shift 0, blockStep = [0xAB, 0x718F]).
 *
 * Physical layout (headerSize fixed at 0x1000, firstHashTableAddress =
 * 0x1000; unlisted bytes read as zero):
 *   0x0000..0x1000: header (magic, header_size) + volume descriptor
 *   0x1000..0x2000: backing block 0 - group 0's level-0 hash table; only
 *                   record 0 written (the file-table block's hash entry)
 *   0x2000..0x3000: backing block 1 - file-table block (logical block 0),
 *                   one entry for `fileName`, startingBlockNum = 0x70E4
 *   0xAC000..0xAD000: backing block 0xAB - the 2-entry top table for
 *                   topLevel == Two
 *   0x7191000..0x7192000: backing block 0x7190 - top-group 1's level-1
 *                   subtable; only record 0 written
 *   0x7192000..0x7193000: backing block 0x7191 - level-0 subtable entry
 *                   for logical block 0x70E4 (0x70E4 % 0xAA == 0)
 *   0x7193000..0x7194000: backing block 0x7192 - file data (logical
 *                   block 0x70E4) - the XEX2 stub
 */
export interface StfsLevelTwoFixtureOptions {
	magic?: 'CON ' | 'LIVE' | 'PIRS';
	titleId?: number;
	version?: number;
	fileName?: string;
}

export interface SparseStfsFixture {
	/** Total logical file size to pass as inspectSource's fileSize - not
	 * the number of bytes actually held in memory. */
	length: number;
	/** Serves zeros for any byte this builder never wrote. */
	readFn: (offset: number, length: number) => Uint8Array;
}

export function makeStfsLevelTwoFixture(
	opts: StfsLevelTwoFixtureOptions = {},
): SparseStfsFixture {
	const magic = opts.magic ?? 'CON ';
	const headerSize = 0x1000;
	const blockSeparation = 1; // shift 0
	const titleId = opts.titleId ?? 0x5a5a0001;
	const version = opts.version ?? 1;
	const fileName = opts.fileName ?? 'default.xex';

	if (fileName.length === 0 || fileName.length > 0x28) {
		throw new Error(
			`fileName must be 1-40 ASCII characters, got length ${fileName.length}`,
		);
	}

	const ALLOCATED_BLOCK_COUNT = 0x70e5; // 1 past 0x70E4 -> forces topLevel == Two
	const FILE_TABLE_BLOCK = 0; // logical
	const DATA_BLOCK = LEVEL1_GROUP_SIZE; // logical - first block of top-group 1
	const [BLOCK_STEP0, BLOCK_STEP1] = BLOCK_STEP_SHIFT0;
	const shift = ~blockSeparation & 1;
	const firstHashTableAddress = (headerSize + 0xfff) & 0xfffff000;

	const fileTableAddr = blockToFileAddress(
		computeBackingDataBlockNumber(FILE_TABLE_BLOCK, shift),
		firstHashTableAddress,
	);
	const dataAddr = blockToFileAddress(
		computeBackingDataBlockNumber(DATA_BLOCK, shift),
		firstHashTableAddress,
	);
	const group0HashAddr = blockToFileAddress(
		computeLevel0BackingHashBlockNumber(FILE_TABLE_BLOCK, shift, BLOCK_STEP0),
		firstHashTableAddress,
	);
	const topTableAddr = blockToFileAddress(
		computeLevel1BackingHashBlockNumber(0, shift, BLOCK_STEP0, BLOCK_STEP1),
		firstHashTableAddress,
	);
	const level1SubtableAddr = blockToFileAddress(
		computeLevel1BackingHashBlockNumber(
			DATA_BLOCK,
			shift,
			BLOCK_STEP0,
			BLOCK_STEP1,
		),
		firstHashTableAddress,
	);
	const level0SubtableAddr = blockToFileAddress(
		computeLevel0BackingHashBlockNumber(DATA_BLOCK, shift, BLOCK_STEP0),
		firstHashTableAddress,
	);

	const sparse = new SparseChunks();
	// header (abs 0x0)
	sparse.writeAt(0, (buf, view) => {
		writeAscii(buf, 0, magic);
		view.setUint32(HEADER_SIZE_FIELD_OFFSET, headerSize, false);
		const VD = VOLUME_DESCRIPTOR_OFFSET;
		buf[VD + 0] = 0x24; // descriptor's own size
		buf[VD + 1] = 0;
		buf[VD + 2] = blockSeparation;
		view.setUint16(VD + 3, 1, true);
		writeInt24LE(view, VD + 5, FILE_TABLE_BLOCK);
		view.setUint32(VD + 0x1c, ALLOCATED_BLOCK_COUNT, false);
		view.setUint32(VD + 0x20, 0, false);
	});

	// group 0's level-0 hash table: block 0's hash entry
	sparse.writeHashEntryAt(group0HashAddr);

	// file-table block (logical block 0)
	sparse.writeAt(fileTableAddr, (buf, view, local) => {
		writeAscii(buf, local, fileName);
		buf[local + 0x28] = fileName.length;
		writeInt24LE(view, local + 0x29, 1);
		writeInt24LE(view, local + 0x2c, 1); // copy of 0x29 (blocksForFile)
		writeInt24LE(view, local + 0x2f, DATA_BLOCK);
		view.setUint16(local + 0x32, 0xffff, false);
		view.setUint32(local + 0x34, 0x100, false); // fileSize (xexDeclaredSize)
	});

	// top table (2 entries)
	sparse.writeHashEntryAt(topTableAddr);
	sparse.writeHashEntryAt(topTableAddr + HASH_ENTRY_SIZE);

	// top-group 1's level-1 subtable (record 0)
	sparse.writeHashEntryAt(level1SubtableAddr);

	// level-0 subtable entry for logical block 0x70E4 (record 0)
	sparse.writeHashEntryAt(level0SubtableAddr);

	// file data (logical block 0x70E4)
	sparse.writeAt(dataAddr, (buf, view, local) => {
		writeXexStub(buf, view, local, { titleId, version });
	});

	return sparse.toFixture(dataAddr + 0x1000);
}

/** Writes a hash-tree record with an explicit next-block pointer, for
 * chaining file-table block 0 to file-table block 1 (`writeHashEntry` is
 * the always-terminated variant used elsewhere). */
function writeHashEntryChained(
	buf: Uint8Array,
	view: DataView,
	localOffset: number,
	nextBlock: number,
): void {
	buf[localOffset + STATUS_BYTE_OFFSET] = 0x80; // status: allocated
	writeInt24BE(view, localOffset + NEXT_BLOCK_OFFSET, nextBlock);
}

export interface StfsMultiBlockListingFixtureOptions {
	magic?: 'CON ' | 'LIVE' | 'PIRS';
	titleId?: number;
	version?: number;
}

/**
 * topLevel == Zero fixture whose file listing spans **two** file-table
 * blocks, chained via block 0's hash-entry next-block pointer - exercises
 * the reader's block-to-block walk when following that chain.
 *
 * Block 0 (logical) holds one decoy entry, "readme.txt", in its first
 * slot, proving the reader doesn't stop at the first populated block.
 * Block 1 holds "default.xex" in its first slot. Block 2 is the XEX2 stub
 * data.
 */
export function makeStfsMultiBlockListingFixture(
	opts: StfsMultiBlockListingFixtureOptions = {},
): Uint8Array {
	const magic = opts.magic ?? 'CON ';
	const headerSize = 0x1000;
	const blockSeparation = 1;
	const titleId = opts.titleId ?? 0x5a5a0001;
	const version = opts.version ?? 1;

	const FILE_TABLE_BLOCK_A = 0;
	const FILE_TABLE_BLOCK_B = 1;
	const DATA_BLOCK = 2;
	const ALLOCATED_BLOCK_COUNT = 3;
	const shift = ~blockSeparation & 1;
	const firstHashTableAddress = (headerSize + 0xfff) & 0xfffff000;

	function blockToAddress(blockNum: number): number {
		return blockToFileAddress(
			computeBackingDataBlockNumber(blockNum, shift),
			firstHashTableAddress,
		);
	}
	function hashAddressOfBlock(blockNum: number): number {
		const level0 = computeLevel0BackingHashBlockNumber(
			blockNum,
			shift,
			BLOCK_STEP_SHIFT0[0],
		);
		return (
			(level0 << 0xc) +
			firstHashTableAddress +
			(blockNum % LEVEL0_GROUP_SIZE) * HASH_ENTRY_SIZE +
			((blockSeparation & 2) << 0xb)
		);
	}

	const tableAAddr = blockToAddress(FILE_TABLE_BLOCK_A);
	const tableBAddr = blockToAddress(FILE_TABLE_BLOCK_B);
	const dataAddr = blockToAddress(DATA_BLOCK);
	const xexDeclaredSize = 0x100;
	const totalSize = dataAddr + 0x1000;
	const buf = new Uint8Array(totalSize);
	const view = new DataView(buf.buffer);

	// header
	writeAscii(buf, 0, magic);
	view.setUint32(HEADER_SIZE_FIELD_OFFSET, headerSize, false);

	// volume descriptor @ 0x379
	const VD = VOLUME_DESCRIPTOR_OFFSET;
	buf[VD + 0] = 0x24; // descriptor's own size
	buf[VD + 2] = blockSeparation;
	view.setUint16(VD + 3, 2, true); // fileTableBlockCount = 2
	writeInt24LE(view, VD + 5, FILE_TABLE_BLOCK_A);
	view.setUint32(VD + 0x1c, ALLOCATED_BLOCK_COUNT, false);

	// hash entries: A chains to B, B and DATA terminate
	writeHashEntryChained(
		buf,
		view,
		hashAddressOfBlock(FILE_TABLE_BLOCK_A),
		FILE_TABLE_BLOCK_B,
	);
	writeHashEntry(buf, view, hashAddressOfBlock(FILE_TABLE_BLOCK_B));
	writeHashEntry(buf, view, hashAddressOfBlock(DATA_BLOCK));

	// file-table block A: one decoy entry
	writeAscii(buf, tableAAddr, 'readme.txt');
	buf[tableAAddr + 0x28] = 'readme.txt'.length;
	writeInt24LE(view, tableAAddr + 0x2f, DATA_BLOCK); // harmless - never read
	view.setUint16(tableAAddr + 0x32, 0xffff, false); // pathIndicator = root
	view.setUint32(tableAAddr + 0x34, 0, false); // fileSize = 0

	// file-table block B: default.xex
	writeAscii(buf, tableBAddr, 'default.xex');
	buf[tableBAddr + 0x28] = 'default.xex'.length;
	writeInt24LE(view, tableBAddr + 0x29, 1); // blocksForFile
	writeInt24LE(view, tableBAddr + 0x2c, 1); // copy of 0x29 (blocksForFile)
	writeInt24LE(view, tableBAddr + 0x2f, DATA_BLOCK);
	view.setUint16(tableBAddr + 0x32, 0xffff, false);
	view.setUint32(tableBAddr + 0x34, xexDeclaredSize, false);

	// data block: XEX2 stub
	writeXexStub(buf, view, dataAddr, { titleId, version });

	return buf;
}

// ---------------------------------------------------------------------
// Write-side layout constants and helpers.
//
// Everything above mirrors the STFS *read* layout, for fixtures fed INTO
// a conversion session as a source. Everything below mirrors the *write*
// layout - where things land in the bytes a session targeting
// `{ format: 'stfs' }` produces. Not exposed anywhere else, so mirrored
// here by hand.
// ---------------------------------------------------------------------

/** Default header size the write session emits (0x971A bytes of actual
 * metadata, padded to the next block boundary - see
 * STFS_WRITE_FIRST_HASH_TABLE_ADDRESS below). */
export const STFS_WRITE_DEFAULT_HEADER_SIZE = 0x971a;
const STFS_WRITE_BLOCK_SIZE = 0x1000;
/** Block separation the write session always uses; there's no override. */
export const STFS_WRITE_DEFAULT_BLOCK_SEPARATION = 0x00;

/**
 * The on-disk header region is padded from `STFS_WRITE_DEFAULT_HEADER_SIZE`
 * out to the next block boundary before the body (hash tables / file
 * table / file data) begins. This is what every block after the header
 * is actually aligned to - use this, not `STFS_WRITE_DEFAULT_HEADER_SIZE`,
 * when locating anything in the body.
 */
export const STFS_WRITE_FIRST_HASH_TABLE_ADDRESS =
	(STFS_WRITE_DEFAULT_HEADER_SIZE + 0xfff) & 0xfffff000; // 0xA000

/** Absolute offset of the header's TITLE_ID field (BE u32). Write-only -
 * the reader gets titleId from the source's XEX2 stub instead, so there's
 * no read-side offset to dedupe against. */
export const STFS_WRITE_TITLE_ID_OFFSET = 0x360;
/** Absolute offset of the header's fixed-width UTF-16BE DISPLAY_NAME
 * field. Same offset as `DISPLAY_NAME_FIELD_OFFSET` above - kept as a
 * separate export since existing write-side tests already import this
 * name. */
export const STFS_WRITE_DISPLAY_NAME_OFFSET = 0x411;

export const STFS_WRITE_CONSOLE_ID_OFFSET = 0x36c;
export const STFS_WRITE_CONSOLE_ID_LEN = 5;
export const STFS_WRITE_PROFILE_ID_OFFSET = 0x371;
export const STFS_WRITE_PROFILE_ID_LEN = 8;
export const STFS_WRITE_ONLINE_CREATOR_OFFSET = 0x3ad;
export const STFS_WRITE_ONLINE_CREATOR_LEN = 8;
export const STFS_WRITE_DEVICE_ID_OFFSET = 0x3fd;
export const STFS_WRITE_DEVICE_ID_LEN = 20;
/** Same offset/layout as `AVATAR_ITEM_METADATA_FIELD_OFFSET` above -
 * kept as a separate export since existing write-side tests already
 * import the `STFS_WRITE_*` naming for this region's sibling fields. */
export const STFS_WRITE_AVATAR_ITEM_METADATA_OFFSET = 0x3d9;

/**
 * Known ContentType discriminants, keyed to match the `contentType`
 * string union used elsewhere. `gamesOnDemand` also doubles as the write
 * session's fallback default when no override/launch-executable/source
 * content type is available. `unrecognized` (0x00FF0000) isn't a real
 * discriminant (not one of Velocity's StfsConstants.h values) - it stands
 * in for "a content type this codebase doesn't recognize".
 */
export const STFS_CONTENT_TYPE = {
	xbox360Title: 0x1000,
	installedGame: 0x4000,
	xboxOriginal: 0x5000,
	gamesOnDemand: 0x7000,
	theme: 0x30000,
	gamerPicture: 0x20000,
	gameDemo: 0x80000,
	movie: 0x100000,
	arcadeGame: 0xd0000,
	xna: 0xe0000,
	communityGame: 0x2000000,
	// Title-attached family - each names a *parent* title via titleId
	// rather than being bootable itself.
	savedGame: 1,
	marketPlaceContent: 2,
	xboxSavedGame: 0x60000,
	avatarItem: 0x9000,
	installer: 0xb0000,
	// ProfileAccount family - a root-level obfuscated `Account` file
	// (see fixtures/account.ts's makeAccountFileBytes). On a real
	// console this sits at Content/<profileId>/<title-ID>/00010000/<id>
	profile: 0x10000,
	unrecognized: 0x00ff0000,
} as const;

/**
 * Byte offsets, within a drained STFS write session's output, of the
 * `createdTimeStamp`/`accessTimeStamp` fields for the *sole* file entry
 * of a minimal single-file fixture (one top-level file, no
 * subdirectories, so the file table is one entry in one block).
 *
 * Both fields are stamped from a single wall-clock reading taken once per
 * session open, written to both offset 0x38 and 0x3C within the entry.
 * That's expected, not a bug: two independently-opened sessions over
 * identical input can disagree on these two fields if they straddle a
 * millisecond boundary, so a determinism check across separate opens
 * should skip only these two 4-byte windows.
 *
 * Derivation: output chunk 0 is the header, padded to
 * `firstHashTableAddress` = 0xA000 (STFS_WRITE_FIRST_HASH_TABLE_ADDRESS).
 * Default block separation gives a shift of 1, so logical block 0's
 * physical position is `(1 << 1) + 0` = 2 - two level-0 hash-table blocks
 * (each BLOCK_SIZE) always precede it. The file table is one entry in one
 * block, so that block *is* logical block 0, and the fixture's one file
 * sits at local offset 0 within it. Absolute offset = header + 2 hash
 * blocks + field offset = `0xA000 + 2*0x1000 + 0x38` = `0xC038`
 * (createdTimeStamp), `0xC03C` (accessTimeStamp).
 */
export function stfsMinimalFixtureTimestampOffsets(): {
	createdTimestamp: number;
	accessTimestamp: number;
} {
	const firstHashTableAddress =
		(STFS_WRITE_DEFAULT_HEADER_SIZE + 0xfff) & 0xfffff000;
	const sexShift = ~STFS_WRITE_DEFAULT_BLOCK_SEPARATION & 1;
	const fileTablePhysicalBlock = (1 << sexShift) + 0; // backing block for logical block 0
	const fileTableStart =
		firstHashTableAddress + fileTablePhysicalBlock * STFS_WRITE_BLOCK_SIZE;
	return {
		createdTimestamp: fileTableStart + 0x38,
		accessTimestamp: fileTableStart + 0x3c,
	};
}
