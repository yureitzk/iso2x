import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { makeReadFn, nameMap } from '../../utils/read-fns.js';
import {
	convertXisoFixtureToBytes,
	driveAndDrain,
	makePart,
	splitCisoAt,
} from '../../utils/session-helpers.js';
import {
	ConversionSession,
	cisoSectorSize,
	cisoFilePaddingModulus,
	cisoFileSplitPoint,
	resolveBatchEntry,
} from '../../../dist/index.js';
import { resolveArbitraryXisoSplit } from '../../../dist/detect-advanced.js';
import { CISO_SOURCE } from '../../utils/sources.js';

let SECTOR_SIZE: number;
let FILE_SPLIT_POINT: number;
let FILE_PADDING_MODULUS: number;
let cisoBytes: Uint8Array;
let part1: Uint8Array;
let part2: Uint8Array;

beforeAll(async () => {
	await setupWasm();
	SECTOR_SIZE = cisoSectorSize();
	FILE_SPLIT_POINT = cisoFileSplitPoint();
	FILE_PADDING_MODULUS = cisoFilePaddingModulus();

	const xiso = makeFixture({ titleId: 0x41560002 });
	cisoBytes = convertXisoFixtureToBytes(xiso, {
		format: 'ciso',
		outputName: 'game',
	});
	const view = new DataView(
		cisoBytes.buffer,
		cisoBytes.byteOffset,
		cisoBytes.length,
	);
	const totalDataSectors = Number(
		view.getBigUint64(8, true) / BigInt(view.getUint32(16, true)),
	);
	const splitSector = Math.max(1, Math.floor(totalDataSectors / 2));
	({ part1, part2 } = splitCisoAt(cisoBytes, splitSector));
});

describe('ConversionSession(ciso source) with a real split across two parts', () => {
	it('reconstructs the full logical sector stream identically to the unsplit single-part source', () => {
		const fromSplit = driveAndDrain(
			ConversionSession.open(
				makeReadFn(part1), // fallback readFn/size - unused once sourceParts is given
				part1.length,
				{ format: 'xiso' },
				{
					source: CISO_SOURCE.source,
					parts: [makePart('game.1.cso', part1), makePart('game.2.cso', part2)],
				},
			),
			64 * SECTOR_SIZE,
		);
		const fromUnsplit = driveAndDrain(
			ConversionSession.open(
				makeReadFn(cisoBytes),
				cisoBytes.length,
				{ format: 'xiso' },
				CISO_SOURCE,
			),
			64 * SECTOR_SIZE,
		);
		expect(fromSplit).toEqual(fromUnsplit);
	});

	it('throws at open() if fewer parts are provided than the index table implies', () => {
		expect(() =>
			ConversionSession.open(
				makeReadFn(part1),
				part1.length,
				{ format: 'xiso' },
				{
					source: CISO_SOURCE.source,
					parts: [makePart('game.1.cso', part1)],
				},
			),
		).toThrow(/index table implies 2 part\(s\), but only 1 were provided/i);
	});
});

// ---------------------------------------------------------------------------
// detect.ts's arbitrary-filename split detection only ever looks at files
// that detect as 'xiso' by magic - CISO parts detect as 'ciso' and carry
// their own self-describing index table, so that logic must never try to
// reinterpret a CISO split as a raw XISO fragment pair, named-convention or
// not. See xiso/splitting.test.ts for the detection logic itself.
describe('arbitrary-filename split detection does not misfire on CISO parts', () => {
	it('resolveArbitraryXisoSplit ignores files that detect as ciso, even under arbitrary names', async () => {
		const files = {
			'arbitrary-name-one.bin': part1,
			'arbitrary-name-two.bin': part2,
		};
		const result = await resolveArbitraryXisoSplit(
			Object.keys(files),
			nameMap(files),
		);
		expect(result).toBeNull();
	});

	it('resolveBatchEntry does not recognize a CISO split under arbitrary names (these names miss the ".1."/".2." naming convention, and CISO has no content-based detector to fall back on)', async () => {
		const files = {
			'arbitrary-name-one.bin': part1,
			'arbitrary-name-two.bin': part2,
		};
		const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));
		// Falls through to the single-file fallback: part 1 alone,
		// correctly still detected as 'ciso' by magic, just not paired up.
		expect(resolved.kind).toBe('file');
		if (resolved.kind !== 'file') throw new Error('unreachable');
		expect(resolved.format).toBe('ciso');
	});
});

// ---------------------------------------------------------------------------
// Format-specific splits: naming-convention detection for CISO's
// ".1.cso"/".2.cso" pair (formats::iso::resolve_batch_entry's
// detect_named_split, via find_named_split - see split_detect.rs). Unlike
// CCI (where each part is fully self-contained with its own header and
// index), CISO's header and full index table live only in part 1 (read.rs:
// CisoSource::open() reads them from the front of parts[0] and never
// touches part 2 until an actual read_sector() call) - detect_named_split
// verifies by opening the real CisoSource::open() constructor over both
// parts, but at open() time that only really exercises part 1's header/
// index and the part-count check the "fewer parts than the index table
// implies" test above already covers directly.
describe('resolveBatchEntry ".1."/".2." named-split detection (CISO)', () => {
	it('reports Dir for a valid ".1.cso"/".2.cso" named pair', async () => {
		const files = {
			'game.1.cso': part1,
			'game.2.cso': part2,
		};
		const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));
		expect(resolved.kind).toBe('dir');
		if (resolved.kind !== 'dir') throw new Error('unreachable');
		expect(resolved.format).toBe('ciso');
		expect(resolved.parts.map((p) => p.name).sort()).toEqual([
			'game.1.cso',
			'game.2.cso',
		]);
	});

	it('reports Invalid for a named pair where part 1 has a bad magic', async () => {
		// CisoSource::open() reads the header and full index table from
		// the front of part 1 only (read.rs: CSOHeader::deserialize over
		// the first 24 bytes, then header.index_table_len() * 4 bytes of
		// index) - it never touches part 2's content at open() time, and
		// part 1's actual sector data (after the header/index) is only
		// read lazily via read_sector(). So the only open()-time failure a
		// named-pair corruption can reliably trigger is breaking part 1's
		// header itself; corrupting part 2, or truncating the tail of
		// either part, doesn't touch anything open() inspects.
		const corrupt = part1.slice();
		corrupt[0] = 0x00;
		const files = {
			'game.1.cso': corrupt,
			'game.2.cso': part2,
		};
		const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));
		expect(resolved.kind).toBe('invalid');
		if (resolved.kind !== 'invalid') throw new Error('unreachable');
		expect(resolved.names.slice().sort()).toEqual(['game.1.cso', 'game.2.cso']);
		expect(resolved.reason).toMatch(/don't form a valid Ciso split/i);
	});

	it('a lone "game.1.cso" without its "game.2.cso" pair falls back to plain single-file detection, not Invalid', async () => {
		// find_named_split requires both names to be present before
		// detect_named_split ever calls CisoSource::open - a missing
		// sibling is not the same failure mode as a present-but-corrupted
		// one, and must not be reported as Invalid.
		const files = { 'game.1.cso': part1 };
		const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));
		expect(resolved.kind).toBe('file');
		if (resolved.kind !== 'file') throw new Error('unreachable');
		expect(resolved.format).toBe('ciso');
	});
});

// ---------------------------------------------------------------------------
// Actually crossing FILE_SPLIT_POINT (~4.28 GB) on the *write* side can't be
// exercised end-to-end here: it would require a fixture whose compressed
// output exceeds ~4.28 GB, impractical to generate or compress in a unit
// test. That side's split arithmetic is covered directly in Rust unit tests
// in ciso.rs's `split_tests` module instead, against synthetic sizes - the
// same approach cci.rs and xiso.rs's `split_tests`/`tests` modules use for
// their own (differently-shaped) formats.
//
// The *read* side doesn't have that limitation and is covered for real just
// above: part_boundaries/locate_part only need an index table that resets,
// not an actual multi-GB file, so cutting a small real conversion in two
// exercises the genuine reader path end-to-end.
describe('ConversionSession(ciso) splitting - coverage notes', () => {
	it('cisoFileSplitPoint() matches the documented ~4.28 GB threshold', () => {
		expect(FILE_SPLIT_POINT).toBe(0xffbf6000);
	});
	it('cisoFilePaddingModulus() matches the documented 0x400 (ciso.py pad_file_size)', () => {
		expect(FILE_PADDING_MODULUS).toBe(0x400);
	});
});
