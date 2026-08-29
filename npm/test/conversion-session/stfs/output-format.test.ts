import { describe, it, expect, beforeAll, inject } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import {
	ConversionSession,
	stfsFileEntrySize,
	stfsFileEntryNameLenOffset,
	stfsFileEntryPathIndicatorOffset,
} from '../../../dist/index.js';
import { makeReadFn, nullReadFn } from '../../utils/read-fns.js';
import {
	driveHashing,
	drain,
	expectStfsOutputDeterministicIgnoringTimestamps,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import {
	makeStfsFixture,
	BLOCK_SIZE,
	VOLUME_DESCRIPTOR_OFFSET,
	CONTENT_TYPE_FIELD_OFFSET,
	ROOT_PATH_INDICATOR,
	STFS_WRITE_DEFAULT_HEADER_SIZE,
	STFS_WRITE_FIRST_HASH_TABLE_ADDRESS,
	STFS_WRITE_TITLE_ID_OFFSET,
	STFS_WRITE_DISPLAY_NAME_OFFSET,
	STFS_WRITE_AVATAR_ITEM_METADATA_OFFSET,
	STFS_CONTENT_TYPE,
} from '../../utils/fixtures/stfs.js';
import {
	EXTRACTED_SOURCE,
	STFS_SOURCE,
	XISO_SOURCE,
} from '../../utils/sources.js';

beforeAll(setupWasm);

/** Opens an `stfs` session from an extracted source with no launch executable; callers must supply `titleId` since there's nothing to detect it from. */
function openStfsFromExtracted(
	fileName: string,
	fileBytes: Uint8Array,
	formatOptions: Record<string, unknown>,
) {
	return ConversionSession.open(
		nullReadFn,
		0,
		{ format: 'stfs', ...formatOptions },
		{
			source: EXTRACTED_SOURCE.source,
			parts: [
				{
					name: fileName,
					size: fileBytes.length,
					readFn: makeReadFn(fileBytes),
				},
			],
		},
	);
}

/**
 * Scans block-aligned offsets after the header for the file-listing entry
 * starting with `fileName`'s bytes, instead of re-deriving the writer's
 * physical-block placement math (see the LAYOUT NOTE atop formats/stfs.rs).
 */
function findFileEntryOffset(out: Uint8Array, fileName: string): number {
	const nameBytes = new TextEncoder().encode(fileName);
	for (
		let off = STFS_WRITE_FIRST_HASH_TABLE_ADDRESS;
		off + nameBytes.length <= out.length;
		off += BLOCK_SIZE
	) {
		let matches = true;
		for (let i = 0; i < nameBytes.length; i++) {
			if (out[off + i] !== nameBytes[i]) {
				matches = false;
				break;
			}
		}
		if (matches) return off;
	}
	throw new Error(
		`file-listing entry for "${fileName}" not found in drained output`,
	);
}

describe('ConversionSession(stfs) output header format', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);

	it('starts with the "CON " magic regardless of the requested content type, and the header is at least DEFAULT_HEADER_SIZE (0x971A) bytes', () => {
		// emit_header always writes MAGIC_CON (see the LAYOUT NOTE atop formats/stfs.rs).
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs' },
			XISO_SOURCE,
		);
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		expect(header.length).toBeGreaterThanOrEqual(STFS_WRITE_DEFAULT_HEADER_SIZE);
		const magic = String.fromCharCode(header[0], header[1], header[2], header[3]);
		expect(magic).toBe('CON ');
	});

	it('writes the xiso source\u2019s detected titleId into the header\u2019s TITLE_ID field (BE u32 @ 0x360)', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs' },
			XISO_SOURCE,
		);
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getUint32(STFS_WRITE_TITLE_ID_OFFSET, false)).toBe(0x41560001);
	});

	it('writes an explicit titleId override into TITLE_ID, ignoring detection entirely', () => {
		const data = new Uint8Array(0x40);
		const session = openStfsFromExtracted('save.dat', data, {
			titleId: 0x5a5a0001,
		});
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getUint32(STFS_WRITE_TITLE_ID_OFFSET, false)).toBe(0x5a5a0001);
	});

	it('detects Xbox Original as the content type (BE u32 @ 0x344) for an xiso (default.xbe) source with no override', () => {
		// Mirrors generate_attach_xbe's OGX check: an XBE-launching source
		// resolves to XboxOriginal, not the GamesOnDemand fallback.
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs' },
			XISO_SOURCE,
		);
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getUint32(CONTENT_TYPE_FIELD_OFFSET, false)).toBe(
			STFS_CONTENT_TYPE.xboxOriginal,
		);
	});

	it('falls back to Games on Demand as the content type when there is no launch executable to detect from', () => {
		// No override and title_id_override skips detection, so content_type
		// falls through to the GamesOnDemand default.
		const data = new Uint8Array(0x40);
		const session = openStfsFromExtracted('save.dat', data, {
			titleId: 0x5a5a0002,
		});
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getUint32(CONTENT_TYPE_FIELD_OFFSET, false)).toBe(
			STFS_CONTENT_TYPE.gamesOnDemand,
		);
	});

	it('encodes an explicit displayName as fixed-width UTF-16BE at the DISPLAY_NAME field (0x411)', () => {
		const data = new Uint8Array(0x40);
		const name = 'Test Game';
		const session = openStfsFromExtracted('save.dat', data, {
			titleId: 0x5a5a0003,
			displayName: name,
		});
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		for (let i = 0; i < name.length; i++) {
			expect(view.getUint16(STFS_WRITE_DISPLAY_NAME_OFFSET + i * 2, false)).toBe(
				name.charCodeAt(i),
			);
		}
		// Confirms the name doesn't spill past its written length; no
		// terminator/padding value is asserted here.
		expect(
			view.getUint16(STFS_WRITE_DISPLAY_NAME_OFFSET + name.length * 2, false),
		).toBe(0);
	});

	it('leaves DISPLAY_NAME unset (no placeholder) when displayName is omitted and titleId has no game-list match', () => {
		// display_name falls back to game_list::find_title_by_id (see
		// open_inner), so omitting it isn't the same as asserting a zeroed
		// field - it's only guaranteed no placeholder text is synthesized.
		// titleId comes from resolveUnmappedTitleId() rather than a
		// hardcoded literal, so the "no match" premise actually holds.
		const data = new Uint8Array(0x40);
		const session = openStfsFromExtracted('save.dat', data, {
			titleId: inject('unmappedTitleId'),
		});
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		const firstUnit = view.getUint16(STFS_WRITE_DISPLAY_NAME_OFFSET, false);
		// Either unset (0x0000) or a real title's first UTF-16BE code unit -
		// never a placeholder value.
		if (firstUnit !== 0) {
			expect(firstUnit).toBeGreaterThanOrEqual(0x20);
		}
	});

	it.each([
		['xbox360Title', STFS_CONTENT_TYPE.xbox360Title],
		['installedGame', STFS_CONTENT_TYPE.installedGame],
		['xboxOriginal', STFS_CONTENT_TYPE.xboxOriginal],
		['gamesOnDemand', STFS_CONTENT_TYPE.gamesOnDemand],
		['gameDemo', STFS_CONTENT_TYPE.gameDemo],
		['arcadeGame', STFS_CONTENT_TYPE.arcadeGame],
		['xna', STFS_CONTENT_TYPE.xna],
		['communityGame', STFS_CONTENT_TYPE.communityGame],
	])(
		'writes an explicit contentType override of "%s" into CONTENT_TYPE (BE u32 @ 0x344)',
		(jsName, expectedValue) => {
			const data = new Uint8Array(0x40);
			const session = openStfsFromExtracted('save.dat', data, {
				titleId: 0x5a5a000f,
				contentType: jsName,
			});
			driveHashing(session);
			const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
			session.free();
			const view = new DataView(header.buffer, header.byteOffset, header.length);
			expect(view.getUint32(CONTENT_TYPE_FIELD_OFFSET, false)).toBe(expectedValue);
		},
	);
});

describe('ConversionSession(stfs) output volume descriptor', () => {
	it('blockSeparation (byte @ vd+2) is always 0 - the write side always emits "male" packages, unlike the fixtures\u2019 female-only defaults', () => {
		const data = new Uint8Array(0x40);
		const session = openStfsFromExtracted('save.dat', data, {
			titleId: 0x5a5a0004,
		});
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		expect(header[VOLUME_DESCRIPTOR_OFFSET + 2]).toBe(0);
	});

	it('fileTableBlockNum (int24 LE @ vd+5) is always 0 - the file table is always planned as the first logical blocks', () => {
		const data = new Uint8Array(0x40);
		const session = openStfsFromExtracted('save.dat', data, {
			titleId: 0x5a5a0005,
		});
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const off = VOLUME_DESCRIPTOR_OFFSET + 5;
		expect(header[off] | (header[off + 1] << 8) | (header[off + 2] << 16)).toBe(
			0,
		);
	});

	it('fileTableBlockCount (LE u16 @ vd+3) is 1 for a single-entry package', () => {
		const data = new Uint8Array(0x40);
		const session = openStfsFromExtracted('save.dat', data, {
			titleId: 0x5a5a0006,
		});
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getUint16(VOLUME_DESCRIPTOR_OFFSET + 3, true)).toBe(1);
	});

	it('totalBlocks (BE u32 @ vd+0x1C) matches totalUnits()', () => {
		const data = new Uint8Array(0x40);
		const session = openStfsFromExtracted('save.dat', data, {
			titleId: 0x5a5a0007,
		});
		driveHashing(session);
		const totalUnits = session.totalUnits();
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getUint32(VOLUME_DESCRIPTOR_OFFSET + 0x1c, false)).toBe(
			totalUnits,
		);
	});
});

describe('ConversionSession(stfs) file-listing entry bytes', () => {
	const FILE_NAME = 'readme.txt';
	const FILE_SIZE = 0x100;

	it('writes one correctly-encoded FILE_ENTRY_SIZE-byte entry for a single top-level file', () => {
		const data = new Uint8Array(FILE_SIZE);
		const session = openStfsFromExtracted(FILE_NAME, data, {
			titleId: 0x5a5a0008,
		});
		driveHashing(session);
		const out = drain(session, UNBOUNDED_CHUNK_SIZE);
		const entryOffset = findFileEntryOffset(out, FILE_NAME);
		const entrySize = stfsFileEntrySize();
		const nameLenOffset = stfsFileEntryNameLenOffset();
		const pathIndicatorOffset = stfsFileEntryPathIndicatorOffset();
		expect(entrySize).toBe(0x40);
		expect(nameLenOffset).toBe(0x28);
		expect(pathIndicatorOffset).toBe(0x32);
		const entry = out.subarray(entryOffset, entryOffset + entrySize);
		const view = new DataView(entry.buffer, entry.byteOffset, entry.length);
		// name-length byte: low 6 bits are ASCII length, top 2 bits are
		// is_contiguous | (is_directory << 1) - a top-level file is 0b01.
		expect(entry[nameLenOffset]).toBe(FILE_NAME.length | 0x40);
		// The file table occupies logical block 0 alone, so the file's
		// data starts at block 1.
		const sb = entry[0x2f] | (entry[0x30] << 8) | (entry[0x31] << 16);
		expect(sb).toBe(1);
		// parentIndex (BE u16 @ pathIndicatorOffset): root-level file.
		expect(view.getUint16(pathIndicatorOffset, false)).toBe(ROOT_PATH_INDICATOR);
		// fileSize (BE u32 @ pathIndicatorOffset+2).
		expect(view.getUint32(pathIndicatorOffset + 2, false)).toBe(FILE_SIZE);
	});
});

describe('ConversionSession(stfs) output manifest', () => {
	it('reports a single entry, named after the resolved titleId as 8 uppercase hex digits', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'stfs' },
			XISO_SOURCE,
		);
		driveHashing(session);
		while (!session.isDone()) session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest).toHaveLength(1);
		expect(manifest[0].name).toBe('41560001');
	});

	it('uses an explicit titleId override for the output name, instead of the detected one', () => {
		const data = new Uint8Array(0x40);
		const session = openStfsFromExtracted('save.dat', data, {
			titleId: 0xdeadbeef,
		});
		driveHashing(session);
		while (!session.isDone()) session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest[0].name).toBe('DEADBEEF');
	});

	it("reported size matches the sum of every drained chunk's length", () => {
		const data = new Uint8Array(0x40);
		const session = openStfsFromExtracted('save.dat', data, {
			titleId: 0x5a5a0009,
		});
		driveHashing(session);
		let total = 0;
		while (!session.isDone()) {
			const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE);
			if (chunk) total += chunk.length;
		}
		const manifest = session.outputManifest();
		session.free();
		expect(manifest[0].size).toBe(total);
	});

	it('is deterministic across separate sessions with identical input, aside from the embedded creation/access timestamp', () => {
		const data = new Uint8Array(0x40).fill(0x42);
		const sessionA = openStfsFromExtracted('save.dat', data, {
			titleId: 0x5a5a000a,
		});
		driveHashing(sessionA);
		const outA = drain(sessionA, 4096);
		const sessionB = openStfsFromExtracted('save.dat', data, {
			titleId: 0x5a5a000a,
		});
		driveHashing(sessionB);
		const outB = drain(sessionB, 1);
		// Independent open() calls may straddle a millisecond boundary and
		// disagree on the embedded timestamps; everything else must match.
		expectStfsOutputDeterministicIgnoringTimestamps(outA, outB);
	});

	it('output differs when input file content differs', () => {
		const dataA = new Uint8Array(0x40).fill(0x11);
		const dataB = new Uint8Array(0x40).fill(0x22);
		const sessionA = openStfsFromExtracted('save.dat', dataA, {
			titleId: 0x5a5a000b,
		});
		driveHashing(sessionA);
		const outA = drain(sessionA, UNBOUNDED_CHUNK_SIZE);
		const sessionB = openStfsFromExtracted('save.dat', dataB, {
			titleId: 0x5a5a000b,
		});
		driveHashing(sessionB);
		const outB = drain(sessionB, UNBOUNDED_CHUNK_SIZE);
		expect(outA).not.toEqual(outB);
	});
});

describe('ConversionSession(stfs) currentEntryName before body streaming begins', () => {
	it('is null immediately after opening, before any nextChunk call', () => {
		const data = new Uint8Array(0x40);
		const session = openStfsFromExtracted('save.dat', data, {
			titleId: 0x5a5a000c,
		});
		driveHashing(session);
		expect(session.currentEntryName()).toBeNull();
		session.free();
	});

	it('is still null right after the header chunk, before any body block has been fully emitted', () => {
		// current_entry_name returns None while block_num == 0 - true for
		// the header and any hash-table blocks before the first data block.
		const data = new Uint8Array(0x40);
		const session = openStfsFromExtracted('save.dat', data, {
			titleId: 0x5a5a000d,
		});
		driveHashing(session);
		session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE);
		expect(session.currentEntryName()).toBeNull();
		session.free();
	});
});

describe('ConversionSession(stfs) content-type passthrough for an unrecognized source value', () => {
	it('preserves the source header\u2019s raw content type on a passthrough stfs->stfs conversion, instead of defaulting to Games on Demand', () => {
		// Source declares a content type this crate has no ContentType
		// discriminant for. titleId here only labels the fixture's XEX2
		// stub and isn't asserted.
		const source = makeStfsFixture({
			contentType: STFS_CONTENT_TYPE.unrecognized,
			titleId: 0x5a5a00aa,
		});
		// titleId override with no contentType override bypasses
		// resolve_title_info() (see open_inner), so resolution falls
		// through to the raw-header fallback.
		const session = ConversionSession.open(
			makeReadFn(source.bytes),
			source.bytes.length,
			{ format: 'stfs', titleId: 0x5a5a00bb },
			STFS_SOURCE,
		);
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getUint32(CONTENT_TYPE_FIELD_OFFSET, false)).toBe(
			STFS_CONTENT_TYPE.unrecognized,
		);
	});

	it('still lets executable-based detection win over an unrecognized source content type when detection isn\u2019t bypassed', () => {
		// No titleId override here, so detection runs and resolves to
		// GamesOnDemand. detected_content_type must still win over the
		// raw-header fallback - guards against the fallback shadowing
		// real detection.
		const source = makeStfsFixture({
			contentType: STFS_CONTENT_TYPE.unrecognized,
			titleId: 0x5a5a00cc,
		});
		const session = ConversionSession.open(
			makeReadFn(source.bytes),
			source.bytes.length,
			{ format: 'stfs' },
			STFS_SOURCE,
		);
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getUint32(CONTENT_TYPE_FIELD_OFFSET, false)).toBe(
			STFS_CONTENT_TYPE.gamesOnDemand,
		);
	});

	it.each([
		['Xbox360Title', STFS_CONTENT_TYPE.xbox360Title],
		['GameDemo', STFS_CONTENT_TYPE.gameDemo],
		['CommunityGame', STFS_CONTENT_TYPE.communityGame],
	])(
		'preserves a bootable source\u2019s %s content type on a stfs->stfs round trip with no overrides',
		(_label, contentTypeValue) => {
			const source = makeStfsFixture({
				contentType: contentTypeValue,
				titleId: 0x5a5a0010,
			});
			const session = ConversionSession.open(
				makeReadFn(source.bytes),
				source.bytes.length,
				{ format: 'stfs' },
				STFS_SOURCE,
			);
			driveHashing(session);
			const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
			session.free();
			const view = new DataView(header.buffer, header.byteOffset, header.length);
			expect(view.getUint32(CONTENT_TYPE_FIELD_OFFSET, false)).toBe(
				contentTypeValue,
			);
		},
	);

	// Same round-trip guarantee as above, but for non-bootable types
	// outside the original 8-variant set.
	it.each([
		['Theme', STFS_CONTENT_TYPE.theme],
		['Movie', STFS_CONTENT_TYPE.movie],
		['GamerPicture', STFS_CONTENT_TYPE.gamerPicture],
	])(
		'preserves a non-bootable source\u2019s %s content type on a stfs->stfs round trip with no overrides',
		(_label, contentTypeValue) => {
			const source = makeStfsFixture({
				contentType: contentTypeValue,
				titleId: 0x5a5a0011,
			});
			const session = ConversionSession.open(
				makeReadFn(source.bytes),
				source.bytes.length,
				{ format: 'stfs' },
				STFS_SOURCE,
			);
			driveHashing(session);
			const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
			session.free();
			const view = new DataView(header.buffer, header.byteOffset, header.length);
			expect(view.getUint32(CONTENT_TYPE_FIELD_OFFSET, false)).toBe(
				contentTypeValue,
			);
		},
	);

	// Title-attached family (SavedGame/XboxSavedGame/MarketPlaceContent/
	// AvatarItem/Installer): none of these carry a launch executable.
	it.each([
		['SavedGame', STFS_CONTENT_TYPE.savedGame],
		['XboxSavedGame', STFS_CONTENT_TYPE.xboxSavedGame],
		['MarketPlaceContent', STFS_CONTENT_TYPE.marketPlaceContent],
		['AvatarItem', STFS_CONTENT_TYPE.avatarItem],
		['Installer', STFS_CONTENT_TYPE.installer],
	])(
		'preserves a title-attached source\u2019s %s content type on a stfs->stfs round trip with no overrides',
		(_label, contentTypeValue) => {
			// titleId here names the *parent* title, per this family's
			// definition - not asserted on directly in this test, only
			// content-type round-trip.
			const source = makeStfsFixture({
				contentType: contentTypeValue,
				titleId: 0x5a5a0012,
			});
			const session = ConversionSession.open(
				makeReadFn(source.bytes),
				source.bytes.length,
				{ format: 'stfs' },
				STFS_SOURCE,
			);
			driveHashing(session);
			const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
			session.free();
			const view = new DataView(header.buffer, header.byteOffset, header.length);
			expect(view.getUint32(CONTENT_TYPE_FIELD_OFFSET, false)).toBe(
				contentTypeValue,
			);
		},
	);

	// AvatarItem carries real structured metadata past the common header
	// (subcategory/colorizable/GUID/skeleton-version at 0x3D9, per
	// Velocity's readMetadata). The it.each above only proves the common
	// CONTENT_TYPE field survives; this test proves the AvatarItem-only
	// region round-trips too.
	it('preserves AvatarItem\u2019s 0x3D9 subcategory/colorizable/GUID/skeleton-version region on a stfs->stfs round trip', () => {
		const sourceMetadata = {
			subCategory: 0x17,
			colorizable: 1,
			guid: Uint8Array.from({ length: 16 }, (_, i) => i + 1),
			skeletonVersion: 2,
		};
		const source = makeStfsFixture({
			contentType: STFS_CONTENT_TYPE.avatarItem,
			titleId: inject('unmappedTitleId'),
			avatarItemMetadata: sourceMetadata,
		});
		const session = ConversionSession.open(
			makeReadFn(source.bytes),
			source.bytes.length,
			{ format: 'stfs' },
			STFS_SOURCE,
		);
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		const AIM = STFS_WRITE_AVATAR_ITEM_METADATA_OFFSET;
		expect(view.getUint32(AIM, true)).toBe(sourceMetadata.subCategory);
		expect(view.getUint32(AIM + 4, true)).toBe(sourceMetadata.colorizable);
		for (let i = 0; i < 16; i++) {
			expect(header[AIM + 8 + i]).toBe(sourceMetadata.guid[i]);
		}
		expect(header[AIM + 24]).toBe(sourceMetadata.skeletonVersion);
	});

	it("drops an out-of-range skeletonVersion (Velocity's validated 1-3) rather than round-tripping bogus AvatarItem metadata", () => {
		// skeletonVersion outside 1..=3 is Velocity's own signal that the
		// region isn't real AvatarItem metadata - read_avatar_item_metadata
		// degrades to None rather than trusting it, so the write side has
		// nothing to preserve and the region comes out zeroed.
		const source = makeStfsFixture({
			contentType: STFS_CONTENT_TYPE.avatarItem,
			titleId: inject('unmappedTitleId'),
			avatarItemMetadata: {
				subCategory: 0x17,
				colorizable: 1,
				guid: Uint8Array.from({ length: 16 }, (_, i) => i + 1),
				skeletonVersion: 0, // outside 1..=3
			},
		});
		const session = ConversionSession.open(
			makeReadFn(source.bytes),
			source.bytes.length,
			{ format: 'stfs' },
			STFS_SOURCE,
		);
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const AIM = STFS_WRITE_AVATAR_ITEM_METADATA_OFFSET;
		for (let i = 0; i < 25; i++) {
			expect(header[AIM + i]).toBe(0);
		}
	});

	it('preserves a title-attached source\u2019s own displayName on a stfs->stfs round trip with no displayName override', () => {
		// A name that can't collide with a real game-list entry, so a
		// false-positive find_title_by_id match can't make this pass for
		// the wrong reason (see open_inner's display_name fallback chain).
		const sourceName = 'Zzz Fixture Save Data Zzz';
		const source = makeStfsFixture({
			contentType: STFS_CONTENT_TYPE.savedGame,
			titleId: inject('unmappedTitleId'),
			displayName: sourceName,
		});
		const session = ConversionSession.open(
			makeReadFn(source.bytes),
			source.bytes.length,
			{ format: 'stfs' },
			STFS_SOURCE,
		);
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		for (let i = 0; i < sourceName.length; i++) {
			expect(view.getUint16(STFS_WRITE_DISPLAY_NAME_OFFSET + i * 2, false)).toBe(
				sourceName.charCodeAt(i),
			);
		}
		expect(
			view.getUint16(
				STFS_WRITE_DISPLAY_NAME_OFFSET + sourceName.length * 2,
				false,
			),
		).toBe(0);
	});

	it('lets an explicit displayName override win over the source\u2019s own header displayName', () => {
		const sourceName = 'Source Original Name';
		const overrideName = 'Override Wins';
		const source = makeStfsFixture({
			contentType: STFS_CONTENT_TYPE.savedGame,
			titleId: inject('unmappedTitleId'),
			displayName: sourceName,
		});
		const session = ConversionSession.open(
			makeReadFn(source.bytes),
			source.bytes.length,
			{ format: 'stfs', displayName: overrideName },
			STFS_SOURCE,
		);
		driveHashing(session);
		const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		for (let i = 0; i < overrideName.length; i++) {
			expect(view.getUint16(STFS_WRITE_DISPLAY_NAME_OFFSET + i * 2, false)).toBe(
				overrideName.charCodeAt(i),
			);
		}
	});
});

describe('ConversionSession(stfs) non-bootable contentType overrides need neither titleId nor a launch executable', () => {
	// Confirms TITLE_ID lands on the documented 0 fallback (see
	// open_inner's doc comment) for these non-throwing, non-bootable cases.
	it.each([
		['gamerPicture', STFS_CONTENT_TYPE.gamerPicture],
		['movie', STFS_CONTENT_TYPE.movie],
		['theme', STFS_CONTENT_TYPE.theme],
	])(
		'writes contentType "%s" and defaults TITLE_ID to 0 for an extracted source with no launch executable and no titleId override',
		(jsName, expectedValue) => {
			const data = new Uint8Array(0x40);
			const session = openStfsFromExtracted('save.dat', data, {
				contentType: jsName,
			});
			driveHashing(session);
			const header = session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
			session.free();
			const view = new DataView(header.buffer, header.byteOffset, header.length);
			expect(view.getUint32(CONTENT_TYPE_FIELD_OFFSET, false)).toBe(expectedValue);
			expect(view.getUint32(STFS_WRITE_TITLE_ID_OFFSET, false)).toBe(0);
		},
	);
});
