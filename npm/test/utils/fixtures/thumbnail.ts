/**
 * Thumbnail (launch icon) embedding for xsf.ts/stfs.ts fixtures.
 */
import { writeAscii } from './binary-utils.js';

const XPR_MAGIC = 0x30525058; // "XPR0" as an LE u32

// XBE icon section layout (offsets relative to the executable's own
// start, same coordinate space as the XBE stub). Cert ends at
// 0x200 + 0x1D0 = 0x3D0, so 0x400 leaves a clean gap.
const XBE_ICON_SECTION_TABLE_OFFSET = 0x400;
const XBE_ICON_NAME_OFFSET = XBE_ICON_SECTION_TABLE_OFFSET + 56; // 0x438
const XBE_ICON_DATA_OFFSET = 0x460;

/**
 * Declared directory-entry size for a `platform: 'ogx'` fixture with
 * `thumbnail` set, when the caller doesn't pass `xbeDeclaredSize`
 * explicitly - the icon section table + name + XPR0 payload don't fit
 * under the plain default, but this still stays under SECTOR_SIZE
 * (0x800).
 */
export const THUMBNAIL_XBE_DECLARED_SIZE = 0x500;

// XEX thumbnail-resource layout (same coordinate space as the XEX2 stub).
// With a thumbnail embedded, the optional-header field table grows from 1
// entry (ExecutionId) to 4 (+ ImageBaseAddress, ResourceInfo,
// BaseFileFormat), which pushes TitleExecutionInfo from 0x30 to 0x38.
export const XEX_THUMBNAIL_EXECUTION_INFO_OFFSET = 0x38;

/**
 * "Base Address" field id - the reader uses this to override the VA that
 * resource-table entries are relative to. Not the same field as "Load
 * Address" (id `0x00010001`), which is parsed for reference only.
 * https://free60.org/System-Software/Formats/XEX/#header-ids
 */
const XEX_FIELD_ID_IMAGE_BASE_ADDRESS = 0x00010201;
const XEX_FIELD_ID_RESOURCE_INFO = 0x000002ff;
const XEX_FIELD_ID_BASE_FILE_FORMAT = 0x000003ff;
const XEX_ICON_LOAD_ADDRESS = 0x00010000;

// Where the (uncompressed) image body starts, per the BaseFileFormat
// block's implicit compression_type: 0.
const XEX_ICON_CODE_OFFSET = 0x80;
const XEX_ICON_RESOURCE_BLOCK_OFFSET = 0x50;
const XEX_ICON_FILE_FORMAT_BLOCK_OFFSET = 0x68;

/**
 * The reader only accepts a decompressed image starting with `"MZ"` (the
 * DOS-header magic a real XEX's embedded PE starts with), as a sanity
 * check that decryption/decompression actually produced something real.
 * The synthetic image body here is `"MZ\0\0"` followed by the XDBF blob.
 */
const XEX_ICON_MZ_PREFIX = new Uint8Array([0x4d, 0x5a, 0x00, 0x00]); // "MZ\0\0"
const XEX_ICON_XDBF_VA = XEX_ICON_LOAD_ADDRESS + XEX_ICON_MZ_PREFIX.length;

// XDBF layout: 24-byte header (magic, version, entry_count/entry_used,
// free_count/free_used, all BE u32s except the magic), entry table
// starting immediately after at XDBF_HEADER_SIZE. Each entry is
// section(u16) + id(u64) + offset(u32) + size(u32) = 18 bytes.
const XDBF_HEADER_SIZE = 24;
const XDBF_ENTRY_SIZE = 18;
const XDBF_SECTION_IMAGE = 2; // image resource section
const XDBF_THUMB_ID = 0x8000n; // "Thumb" resource id

// Real PNG signature - what a header-embedded Thumbnail/Title Thumbnail
// Image field (STFS/GoD offsets 0x171A/0x571A) must start with to pass
// the reader's validation. Unlike the XBE/XEX-embedded icon helpers above
// (whose output is an XPR0/DXT1 container or a verbatim XDBF resource
// blob, never checked against this), a header field is read directly as
// "is this a PNG", so a fixture that writes into it needs real magic
// bytes, not just any payload.
const HEADER_PNG_MAGIC = new Uint8Array([
	0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
]);

/**
 * Builds a minimal byte string usable as a header-embedded Thumbnail/Title
 * Thumbnail Image: real PNG magic followed by `tag` (so two calls with
 * different tags produce distinguishable, round-trippable bytes for tests
 * that need to tell a package's Thumbnail Image apart from its Title
 * Thumbnail Image). The reader only checks the magic prefix and never
 * decodes further, so the bytes after it don't need to form a real PNG
 * body.
 */
export function makeHeaderThumbnailBytes(tag: string): Uint8Array {
	const tagBytes = new TextEncoder().encode(tag);
	const out = new Uint8Array(HEADER_PNG_MAGIC.length + tagBytes.length);
	out.set(HEADER_PNG_MAGIC, 0);
	out.set(tagBytes, HEADER_PNG_MAGIC.length);
	return out;
}

export interface ThumbnailFixtureOptions {
	/**
	 * Only used for `platform: 'ogx'` - which section carries the icon.
	 * Defaults to `'$$XTIMAGE'` (title/game icon, checked first); pass
	 * `'$$XSIMAGE'` (savegame icon) to exercise its fallback instead.
	 */
	xbeSectionName?: '$$XSIMAGE' | '$$XTIMAGE';
}

/**
 * Writes a `$$XSIMAGE`/`$$XTIMAGE`-shaped icon into an already-written XBE
 * stub: a one-entry section table pointing at a raw XPR0/DXT1 texture
 * container.
 *
 * Fixed header (32 bytes: magic, file_size, header_size, flags
 * encoding resource-count=1/type=4-texture, resource_data_offset=0,
 * unknown=0, texture_misc1, texture_format, texture_res1, texture_res2,
 * texture_size_field=0), followed by the `0xFFFFFFFF` end-of-header
 * marker, then the DXT1 payload.
 *
 * The DXT1 payload is one flat-white 4x4 block: `c0 == c1 == 0xFFFF`
 * (opaque 4-color mode) with every index left at 0, so every pixel
 * decodes to color 0 (white).
 */
export function writeXbeThumbnailSection(
	buf: Uint8Array,
	view: DataView,
	xbeOffset: number,
	xbeBaseAddr: number,
	sectionName: string,
): void {
	const dxt1Block = new Uint8Array([
		0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00,
	]);
	// The end-of-header marker sits at a fixed offset of 32 regardless of
	// header_size; header_size must be at least 36 (fixed fields + marker)
	// for the payload to start right after the marker.
	const headerSize = 36;
	const fileSize = headerSize + dxt1Block.length;
	const xpr = new Uint8Array(fileSize);
	const xprView = new DataView(xpr.buffer);
	xprView.setUint32(0, XPR_MAGIC, true);
	xprView.setUint32(4, fileSize, true);
	xprView.setUint32(8, headerSize, true);
	xprView.setUint32(12, 1 | (4 << 16), true); // flags: count=1, type=4 (texture)
	xprView.setUint32(16, 0, true); // resource_data_offset (must be 0)
	xprView.setUint32(20, 0, true); // unknown (must be 0)
	xpr[24] = 0; // texture_misc1
	xpr[25] = 12; // texture_format = DXT1
	xpr[26] = 0; // texture_res1
	xpr[27] = 2; // texture_res2 -> side = 1 << 2 = 4 (a 4x4 icon)
	xprView.setUint32(28, 0, true); // texture_size_field (must be 0 for power-of-2)
	xprView.setUint32(32, 0xffffffff, true); // end-of-header marker
	xpr.set(dxt1Block, headerSize);
	buf.set(xpr, xbeOffset + XBE_ICON_DATA_OFFSET);

	// Section header table: one entry. raw_address/raw_size are
	// file-relative; name_va is virtual and needs the base_addr
	// translation find_xbe_section applies.
	const table = xbeOffset + XBE_ICON_SECTION_TABLE_OFFSET;
	view.setUint32(table + 0x0c, XBE_ICON_DATA_OFFSET, true); // raw (file) address
	view.setUint32(table + 0x10, xpr.length, true); // raw size
	view.setUint32(table + 0x14, xbeBaseAddr + XBE_ICON_NAME_OFFSET, true); // name address (virtual)

	view.setUint32(xbeOffset + 0x11c, 1, true); // num_sections
	view.setUint32(
		xbeOffset + 0x120,
		xbeBaseAddr + XBE_ICON_SECTION_TABLE_OFFSET,
		true,
	); // section_headers_va

	writeAscii(buf, xbeOffset + XBE_ICON_NAME_OFFSET, sectionName);
	buf[xbeOffset + XBE_ICON_NAME_OFFSET + sectionName.length] = 0;
}

/**
 * Builds a minimal XDBF blob containing exactly one entry: an image
 * resource with the "Thumb" id, whose data is the 4 bytes `"PNG!"` - a
 * stand-in for a complete PNG, since the reader copies the resource
 * bytes out verbatim without decoding them.
 */
function buildSyntheticXdbf(): Uint8Array {
	const dataOffset = XDBF_HEADER_SIZE + XDBF_ENTRY_SIZE;
	const buf = new Uint8Array(dataOffset + 4);
	const view = new DataView(buf.buffer);
	writeAscii(buf, 0, 'XDBF');
	view.setUint32(8, 1, false); // entry_table_len (capacity)
	view.setUint32(12, 1, false); // entry_used
	view.setUint32(16, 0, false); // free_table_len (capacity)
	view.setUint32(20, 0, false); // free_used
	const entry = XDBF_HEADER_SIZE;
	view.setUint16(entry + 0, XDBF_SECTION_IMAGE, false); // section
	view.setBigUint64(entry + 2, XDBF_THUMB_ID, false); // id
	view.setUint32(entry + 10, 0, false); // offset (relative to data start)
	view.setUint32(entry + 14, 4, false); // size
	writeAscii(buf, dataOffset, 'PNG!');
	return buf;
}

/**
 * Writes a thumbnail-bearing resource table into an already-written XEX
 * stub, plus the synthetic image body (MZ prefix + XDBF blob) it points
 * at. Requires the stub to have been written with `executionInfoOffset:
 * XEX_THUMBNAIL_EXECUTION_INFO_OFFSET` so this can grow the field table
 * to 4 entries without overlapping `TitleExecutionInfo`.
 */
export function writeXexThumbnailResource(
	buf: Uint8Array,
	view: DataView,
	xexOffset: number,
): void {
	const xdbf = buildSyntheticXdbf();

	// The "none" compression path reads the image starting at code_offset,
	// so it must point at the synthetic image body written below.
	view.setUint32(xexOffset + 0x08, XEX_ICON_CODE_OFFSET, false);

	// Grow the field table from 1 entry (ExecutionId) to 4, appending
	// ImageBaseAddress/ResourceInfo/BaseFileFormat.
	view.setUint32(xexOffset + 0x14, 4, false); // field_count
	view.setUint32(xexOffset + 0x20, XEX_FIELD_ID_IMAGE_BASE_ADDRESS, false);
	view.setUint32(xexOffset + 0x24, XEX_ICON_LOAD_ADDRESS, false);
	view.setUint32(xexOffset + 0x28, XEX_FIELD_ID_RESOURCE_INFO, false);
	view.setUint32(xexOffset + 0x2c, XEX_ICON_RESOURCE_BLOCK_OFFSET, false);
	view.setUint32(xexOffset + 0x30, XEX_FIELD_ID_BASE_FILE_FORMAT, false);
	view.setUint32(xexOffset + 0x34, XEX_ICON_FILE_FORMAT_BLOCK_OFFSET, false);

	// ResourceInfo block: size(u32) + one { name[8], address(VA, u32),
	// size(u32) } entry. The reader falls back to trying every entry when
	// none matches by title ID, so any 8-byte tag works here.
	const res = xexOffset + XEX_ICON_RESOURCE_BLOCK_OFFSET;
	view.setUint32(res + 0, 20, false); // block size
	writeAscii(buf, res + 4, 'THUMBRES');
	view.setUint32(res + 12, XEX_ICON_XDBF_VA, false); // VA
	view.setUint32(res + 16, xdbf.length, false);

	// BaseFileFormat block: size(u32) + encryption_type(u16) +
	// compression_type(u16). Both left at 0 -> unencrypted, uncompressed.
	const fmt = xexOffset + XEX_ICON_FILE_FORMAT_BLOCK_OFFSET;
	view.setUint32(fmt + 0, 8, false);

	buf.set(XEX_ICON_MZ_PREFIX, xexOffset + XEX_ICON_CODE_OFFSET);
	buf.set(xdbf, xexOffset + XEX_ICON_CODE_OFFSET + XEX_ICON_MZ_PREFIX.length);
}
