import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import {
	makeStfsFixture,
	makeStfsLevelOneFixture,
	makeStfsLevelTwoFixture,
	makeStfsMultiBlockListingFixture,
	STFS_CONTENT_TYPE,
	DESCRIPTOR_TYPE_FIELD_OFFSET,
} from '../utils/fixtures/stfs.js';
import {
	detectFormat,
	inspectSource,
	stfsFileEntrySize,
	stfsFileEntryNameLenOffset,
	stfsFileEntryPathIndicatorOffset,
	SourceRef,
} from '../../dist/index.js';
import { makeReadFn, nullReadFn, throwingReadFn } from '../utils/read-fns.js';

beforeAll(setupWasm);

const STFS_SOURCE: SourceRef = { source: { format: 'stfs' } };

describe('inspectSource with a `stfs` source', () => {
	const { bytes: stfsBytes } = makeStfsFixture({
		titleId: 0x5a5a0001,
		version: 1,
	});
	const readFn = makeReadFn(stfsBytes);

	it('detectFormat resolves the fixture bytes as stfs', () => {
		expect(detectFormat(readFn, stfsBytes.length)).toBe('stfs');
	});

	it('parses without throwing', () => {
		expect(() =>
			inspectSource(readFn, stfsBytes.length, STFS_SOURCE),
		).not.toThrow();
	});

	it('returns correct titleId', () => {
		const info = inspectSource(readFn, stfsBytes.length, STFS_SOURCE);
		expect(info.titleId).toBe('5A5A0001');
	});

	it('titleId is 8 uppercase hex digits', () => {
		const info = inspectSource(readFn, stfsBytes.length, STFS_SOURCE);
		expect(info.titleId).toMatch(/^[0-9A-F]{8}$/);
	});

	it('detects Games on Demand content type (STFS is Xbox 360-only)', () => {
		const info = inspectSource(readFn, stfsBytes.length, STFS_SOURCE);
		expect(info.contentType).toBe('gamesOnDemand');
	});

	it('returns different titleIds for different fixtures', () => {
		const { bytes: stfsBytes2 } = makeStfsFixture({ titleId: 0xdeadbeef });
		const info1 = inspectSource(readFn, stfsBytes.length, STFS_SOURCE);
		const info2 = inspectSource(
			makeReadFn(stfsBytes2),
			stfsBytes2.length,
			STFS_SOURCE,
		);
		expect(info1.titleId).not.toBe(info2.titleId);
	});

	it('accepts a custom fetchSize without changing the result', () => {
		const info = inspectSource(readFn, stfsBytes.length, {
			...STFS_SOURCE,
		});
		expect(info.titleId).toBe('5A5A0001');
	});

	it('produces identical results across repeated explicit-source calls', () => {
		const first = inspectSource(readFn, stfsBytes.length, STFS_SOURCE);
		const second = inspectSource(readFn, stfsBytes.length, STFS_SOURCE);
		expect(second).toEqual(first);
	});

	it('reports the packed major.minor.build.qfe fields', () => {
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a0009,
			version: 0x12345678, // 1.2.13398.120
		});
		const info = inspectSource(makeReadFn(bytes), bytes.length, STFS_SOURCE);
		expect(info.version).toStrictEqual({
			kind: 'xex',
			version: { major: 1, minor: 2, build: 13398, qfe: 120 },
			base: undefined,
		});
	});

	it('reflects the fixture default version (1)', () => {
		const info = inspectSource(readFn, stfsBytes.length, STFS_SOURCE);
		expect(info.version).toStrictEqual({
			kind: 'xex',
			version: { major: 0, minor: 0, build: 0, qfe: 1 },
			base: undefined,
		});
	});

	it('throws when the package has no default.xex at the root', () => {
		const { bytes: noXex } = makeStfsFixture({
			fileName: 'save.dat',
			titleId: 0x5a5a0007,
		});
		expect(() =>
			inspectSource(makeReadFn(noXex), noXex.length, STFS_SOURCE),
		).toThrow(/default\.xbe\/default\.xex/);
	});

	it('throws for a dangling parent index in the file listing', () => {
		const { bytes: raw, fileTableAddr } = makeStfsFixture({
			titleId: 0x5a5a0004,
		});
		const corrupted = new Uint8Array(raw);
		const view = new DataView(corrupted.buffer);
		view.setUint16(
			fileTableAddr + stfsFileEntryPathIndicatorOffset(),
			5, // no entry with entry_index 5 exists
			false,
		);
		expect(() =>
			inspectSource(makeReadFn(corrupted), corrupted.length, STFS_SOURCE),
		).toThrow('dangling parent');
	});

	it("throws when a file's parent index resolves to a non-directory entry", () => {
		const { bytes: raw, fileTableAddr } = makeStfsFixture({
			titleId: 0x5a5a0005,
		});
		const corrupted = new Uint8Array(raw);
		const view = new DataView(corrupted.buffer);
		view.setUint16(
			fileTableAddr + stfsFileEntryPathIndicatorOffset(),
			0, // points at itself - a file, not a directory
			false,
		);
		expect(() =>
			inspectSource(makeReadFn(corrupted), corrupted.length, STFS_SOURCE),
		).toThrow("isn't a directory");
	});

	it('throws when the file listing forms a parent-index cycle that never reaches root', () => {
		const { bytes: raw, fileTableAddr } = makeStfsFixture({
			titleId: 0x5a5a0006,
		});
		const corrupted = new Uint8Array(raw);
		const view = new DataView(corrupted.buffer);
		const ENTRY_SIZE = stfsFileEntrySize();
		const NAME_LEN_OFFSET = stfsFileEntryNameLenOffset();
		const PATH_INDICATOR_OFFSET = stfsFileEntryPathIndicatorOffset();
		function writeDirEntry(
			slot: number,
			name: string,
			parentIndex: number,
		): void {
			const addr = fileTableAddr + slot * ENTRY_SIZE;
			for (let i = 0; i < name.length; i++)
				corrupted[addr + i] = name.charCodeAt(i);
			corrupted[addr + NAME_LEN_OFFSET] = name.length | 0x80; // len | (is_directory << 6)
			view.setUint16(addr + PATH_INDICATOR_OFFSET, parentIndex, false);
		}
		// entry_index 0 is the fixture's default.xex (slot 0). Add two
		// directories at slots 1/2 that point at each other, and repoint
		// default.xex's own parent at one of them - resolve()'s guard is
		// what has to stop this, since the chain never reaches 0xFFFF.
		writeDirEntry(1, 'a', 2);
		writeDirEntry(2, 'b', 1);
		view.setUint16(fileTableAddr + PATH_INDICATOR_OFFSET, 1, false);
		expect(() =>
			inspectSource(makeReadFn(corrupted), corrupted.length, STFS_SOURCE),
		).toThrow('too deep');
	});
});

describe('inspectSource with a `stfs` source: LIVE/PIRS magic variants', () => {
	it('accepts a LIVE-magic package', () => {
		const { bytes: stfsBytes } = makeStfsFixture({
			magic: 'LIVE',
			titleId: 0x11112222,
		});
		const info = inspectSource(
			makeReadFn(stfsBytes),
			stfsBytes.length,
			STFS_SOURCE,
		);
		expect(info.titleId).toBe('11112222');
	});

	it('accepts a PIRS-magic package', () => {
		const { bytes: stfsBytes } = makeStfsFixture({
			magic: 'PIRS',
			titleId: 0x33334444,
		});
		const info = inspectSource(
			makeReadFn(stfsBytes),
			stfsBytes.length,
			STFS_SOURCE,
		);
		expect(info.titleId).toBe('33334444');
	});
});

describe('inspectSource with a `stfs` source: error paths', () => {
	it('throws a JS Error for an invalid (zeroed) package', () => {
		expect(() => inspectSource(nullReadFn, 64 * 1024, STFS_SOURCE)).toThrow();
	});

	it('propagates errors thrown inside the readFn', () => {
		expect(() => inspectSource(throwingReadFn, 64 * 1024, STFS_SOURCE)).toThrow(
			'read error from JS',
		);
	});

	it('throws for a zero file size', () => {
		expect(() => inspectSource(nullReadFn, 0, STFS_SOURCE)).toThrow();
	});

	it('throws if the sourceParts array is empty', () => {
		const { bytes: stfsBytes } = makeStfsFixture();
		expect(() =>
			inspectSource(nullReadFn, stfsBytes.length, {
				source: { format: 'stfs' },
				parts: [],
			}),
		).toThrow();
	});

	// file_size <= header_offset::DEVICE_ID (0x3FD) + 0x14 - a real
	// magic plus enough header bytes to pass read_header_prefix, but too
	// short for StfsReader::open's own upfront size check. Distinct from
	// the "zeroed package" case above, which fails on magic instead.
	it('throws for a file too small to contain a full header, even with a valid magic', () => {
		const { bytes } = makeStfsFixture({ titleId: 0x5a5a000b });
		const truncated = bytes.slice(0, 0x400);
		expect(() =>
			inspectSource(makeReadFn(truncated), truncated.length, STFS_SOURCE),
		).toThrow();
	});
});

// StfsReader::open's own volume-descriptor sanity checks - distinct from
// the file-listing corruption tests above, which all assume a valid
// descriptor and corrupt something downstream of it instead.
describe('inspectSource with a `stfs` source: volume descriptor validation', () => {
	// header_offset::VOLUME_DESCRIPTOR (0x379) + 0x1C - "Total Allocated
	// Block Count" per free60.org's Volume Descriptor table.
	const ALLOCATED_BLOCK_COUNT_OFFSET = 0x379 + 0x1c;

	function withAllocatedBlockCount(value: number): Uint8Array {
		const { bytes } = makeStfsFixture({ titleId: 0x5a5a000c });
		const corrupted = new Uint8Array(bytes);
		new DataView(corrupted.buffer).setUint32(
			ALLOCATED_BLOCK_COUNT_OFFSET,
			value,
			false,
		);
		return corrupted;
	}

	it('throws when allocated_block_count is zero', () => {
		const corrupted = withAllocatedBlockCount(0);
		expect(() =>
			inspectSource(makeReadFn(corrupted), corrupted.length, STFS_SOURCE),
		).toThrow(/no allocated blocks/);
	});

	// One past the Level::Two ceiling (0x4AF768) - StfsReader::open bails
	// out here rather than silently misreading the hash tree.
	it('throws when allocated_block_count exceeds the Level::Two ceiling', () => {
		const corrupted = withAllocatedBlockCount(0x4a_f769);
		expect(() =>
			inspectSource(makeReadFn(corrupted), corrupted.length, STFS_SOURCE),
		).toThrow(/invalid allocated block count/);
	});

	// The exact ceiling value itself must still be accepted - guards
	// against an off-by-one on the boundary check above. (The fixture's
	// actual file/hash-table contents don't back a package this large,
	// so this only exercises the Level::Two branch of the ceiling match,
	// not a full parse - open() is expected to fail downstream once it
	// tries to load a top table this fixture doesn't have. What matters
	// here is that the failure is NOT "invalid allocated block count".)
	it('does not reject the Level::Two ceiling value itself as "invalid"', () => {
		const corrupted = withAllocatedBlockCount(0x4a_f768);
		try {
			inspectSource(makeReadFn(corrupted), corrupted.length, STFS_SOURCE);
		} catch (e) {
			expect(String(e)).not.toMatch(/invalid allocated block count/);
		}
	});
});

describe('inspectSource with a `stfs` source: descriptor type validation', () => {
	// DESCRIPTOR_TYPE_FIELD_OFFSET (0x3A9) is a big-endian u32, not a
	// single byte - see that constant's doc comment. A 1-byte check at
	// the field's start would read the MSB, which is 0x00 for both STFS
	// (0) and SVOD (1), so it would never actually reject anything.
	function withDescriptorType(value: number): Uint8Array {
		const { bytes } = makeStfsFixture({ titleId: 0x5a5a0014 });
		const corrupted = new Uint8Array(bytes);
		new DataView(corrupted.buffer).setUint32(
			DESCRIPTOR_TYPE_FIELD_OFFSET,
			value,
			false,
		);
		return corrupted;
	}

	it('accepts descriptor type 0 (STFS-shaped) - the default every other fixture relies on', () => {
		const stfs = withDescriptorType(0);
		expect(() =>
			inspectSource(makeReadFn(stfs), stfs.length, STFS_SOURCE),
		).not.toThrow();
	});

	it('rejects descriptor type 1 (SVOD-shaped) instead of silently misreading it as STFS', () => {
		const svod = withDescriptorType(1);
		expect(() =>
			inspectSource(makeReadFn(svod), svod.length, STFS_SOURCE),
		).toThrow(/not STFS-shaped/);
	});

	// 1, as a big-endian u32, has its only nonzero byte in the LSB, so
	// the previous test alone wouldn't catch a check that only looks at
	// the MSB. This value's MSB is itself nonzero, closing that gap.
	it('rejects an arbitrary non-STFS descriptor type value', () => {
		const corrupted = withDescriptorType(0xff);
		expect(() =>
			inspectSource(makeReadFn(corrupted), corrupted.length, STFS_SOURCE),
		).toThrow(/not STFS-shaped/);
	});
});

describe('inspectSource with a `stfs` source: topLevel == One fixture', () => {
	const stfsBytes = makeStfsLevelOneFixture({ titleId: 0x5a5a0002 });
	const readFn = makeReadFn(stfsBytes);

	it('parses without throwing', () => {
		expect(() =>
			inspectSource(readFn, stfsBytes.length, STFS_SOURCE),
		).not.toThrow();
	});

	it('returns correct titleId from the group-1 data block', () => {
		const info = inspectSource(readFn, stfsBytes.length, STFS_SOURCE);
		expect(info.titleId).toBe('5A5A0002');
	});

	it('detects Games on Demand content type', () => {
		const info = inspectSource(readFn, stfsBytes.length, STFS_SOURCE);
		expect(info.contentType).toBe('gamesOnDemand');
	});
});

describe('inspectSource with a `stfs` source: topLevel == Two fixture', () => {
	const { length, readFn } = makeStfsLevelTwoFixture({ titleId: 0x5a5a0003 });

	it('parses without throwing', () => {
		expect(() => inspectSource(readFn, length, STFS_SOURCE)).not.toThrow();
	});

	it('returns correct titleId from the group-1 data block', () => {
		const info = inspectSource(readFn, length, STFS_SOURCE);
		expect(info.titleId).toBe('5A5A0003');
	});

	it('detects Games on Demand content type', () => {
		const info = inspectSource(readFn, length, STFS_SOURCE);
		expect(info.contentType).toBe('gamesOnDemand');
	});
});

describe('inspectSource with a `stfs` source: multi-block file listing', () => {
	it('follows the file-table block chain to find default.xex in the second block', () => {
		const stfsBytes = makeStfsMultiBlockListingFixture({ titleId: 0x5a5a0008 });
		const info = inspectSource(
			makeReadFn(stfsBytes),
			stfsBytes.length,
			STFS_SOURCE,
		);
		expect(info.titleId).toBe('5A5A0008');
	});
});

// Title-attached family (SavedGame/XboxSavedGame/MarketPlaceContent/
// AvatarItem/Installer): none of these carry a launch executable, so
// the fixture deliberately doesn't name its data file
// "default.xex"/"default.xbe" - inspectSource must NOT throw despite
// that, unlike the bootable-family "no default.xex" throw test above.
describe('inspectSource with a `stfs` source: title-attached content types', () => {
	it.each([
		['savedGame', STFS_CONTENT_TYPE.savedGame],
		['xboxSavedGame', STFS_CONTENT_TYPE.xboxSavedGame],
		['marketPlaceContent', STFS_CONTENT_TYPE.marketPlaceContent],
		['avatarItem', STFS_CONTENT_TYPE.avatarItem],
		['installer', STFS_CONTENT_TYPE.installer],
	])(
		'reports contentType "%s" and does not require a launch executable to parse',
		(jsName, contentTypeValue) => {
			const { bytes } = makeStfsFixture({
				contentType: contentTypeValue,
				fileName: 'data.bin',
				titleId: 0x5a5a0013,
			});
			const info = inspectSource(makeReadFn(bytes), bytes.length, STFS_SOURCE);
			expect(info.contentType).toBe(jsName);
		},
	);
});
