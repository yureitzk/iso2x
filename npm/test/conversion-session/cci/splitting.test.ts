import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { makeReadFn, nameMap } from '../../utils/read-fns.js';
import {
	convertXisoFixtureToBytes,
	driveAndDrain,
	makePart,
	parseCciLayout,
	splitCciAt,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import {
	ConversionSession,
	cciSectorSize,
	cciFileSplitPoint,
	resolveBatchEntry,
} from '../../../dist/index.js';
import { resolveArbitraryXisoSplit } from '../../../dist/detect-advanced.js';
import type { SourcePart } from '../../../dist/index.js';
import { CCI_SOURCE } from '../../utils/sources.js';

let SECTOR_SIZE: number;
let FILE_SPLIT_POINT: number;
let cciBytes: Uint8Array;
let part1: Uint8Array;
let part2: Uint8Array;
let unsplitDrained: Uint8Array;

/** The genuine two-part split, as `SourcePart[]`. Swap in `p1`/`p2` to build corrupted variants. */
function splitParts(
	p1: Uint8Array = part1,
	p2: Uint8Array = part2,
): SourcePart[] {
	return [makePart('game.1.cci', p1), makePart('game.2.cci', p2)];
}

/**
 * Opens a session against `parts` via the multi-part path. The fallback
 * readFn/size (from `part1`) is unused whenever `parts` is given - see
 * ConversionSession.open's own doc comment - so it's fine to reuse it even
 * for the empty/oversized-parts error tests below.
 */
function openWithParts(parts: SourcePart[]): ConversionSession {
	return ConversionSession.open(
		makeReadFn(part1),
		part1.length,
		{ format: 'xiso' },
		{ source: CCI_SOURCE.source, parts },
	);
}

function drainParts(parts: SourcePart[]): Uint8Array {
	return driveAndDrain(openWithParts(parts), 64 * SECTOR_SIZE);
}

beforeAll(async () => {
	await setupWasm();
	SECTOR_SIZE = cciSectorSize();
	FILE_SPLIT_POINT = cciFileSplitPoint();

	const xiso = makeFixture({ titleId: 0x41560003 });
	cciBytes = convertXisoFixtureToBytes(xiso, {
		format: 'cci',
		outputName: 'game',
	});

	const layout = parseCciLayout(cciBytes);
	const splitSector = Math.max(1, Math.floor(layout.totalSectors / 2));
	({ part1, part2 } = splitCciAt(cciBytes, splitSector));

	unsplitDrained = driveAndDrain(
		ConversionSession.open(
			makeReadFn(cciBytes),
			cciBytes.length,
			{ format: 'xiso' },
			CCI_SOURCE,
		),
		64 * SECTOR_SIZE,
	);
});

describe('ConversionSession(cci source) with a real split across two parts', () => {
	it('reconstructs the full logical sector stream identically to the unsplit single-part source', () => {
		expect(drainParts(splitParts())).toEqual(unsplitDrained);
	});

	it('reconstruction is unaffected by drain chunk size', () => {
		const out1 = driveAndDrain(openWithParts(splitParts()), 1);
		const outAll = driveAndDrain(
			openWithParts(splitParts()),
			UNBOUNDED_CHUNK_SIZE,
		);
		expect(out1).toEqual(outAll);
	});

	it('single-part (unsplit) input still opens and reads fine through the multi-part path', () => {
		expect(drainParts([makePart('game.1.cci', cciBytes)])).toEqual(
			unsplitDrained,
		);
	});

	// Empty sourceParts is rejected by the shared parts_from_js() helper
	// before any format-specific constructor runs, so this message is
	// generic ("sourceParts array must not be empty"), not CCI's own
	// "expected 1 or 2 parts" - that check only ever fires for the
	// too-many-parts case below, since parts_from_js only guards against
	// zero, not CCI's upper bound of two.
	it('throws if no parts are provided', () => {
		expect(() => openWithParts([])).toThrow(
			/sourceParts array must not be empty/i,
		);
	});

	it('throws if the second part has a bad magic', () => {
		const corrupt = part2.slice();
		corrupt[0] = 0x00;
		expect(() => openWithParts(splitParts(part1, corrupt))).toThrow(/bad magic/i);
	});

	it('throws on a header field mismatch (block_size) in a part', () => {
		const corrupt = part1.slice();
		new DataView(corrupt.buffer).setUint32(24, 4096, true); // block_size
		expect(() => openWithParts(splitParts(corrupt))).toThrow(
			/unexpected header fields/i,
		);
	});

	it('throws when index_offset is out of range for a part', () => {
		const corrupt = part1.slice();
		new DataView(corrupt.buffer).setBigUint64(
			16,
			BigInt(corrupt.length + 1000),
			true,
		);
		expect(() => openWithParts(splitParts(corrupt))).toThrow(
			/index_offset .* out of range/i,
		);
	});

	it('throws when the index table is not a whole number of u32 entries', () => {
		// Drop one byte off the end so (size - index_offset) % 4 != 0.
		const corrupt = part1.slice(0, part1.length - 1);
		expect(() => openWithParts(splitParts(corrupt))).toThrow(
			/whole number of u32 entries/i,
		);
	});

	it('throws when the index implies a different sector count than uncompressed_size claims', () => {
		const layout = parseCciLayout(part1);
		const corrupt = part1.slice();
		new DataView(corrupt.buffer).setBigUint64(
			8,
			BigInt((layout.totalSectors + 1) * layout.blockSize),
			true,
		);
		expect(() => openWithParts(splitParts(corrupt))).toThrow(
			/claims .* bytes uncompressed but index implies/i,
		);
	});
});

// detect.ts's arbitrary-filename split detection only ever looks at files
// that detect as 'xiso' by magic - CCI parts detect as 'cci' and carry
// their own self-contained header/index, so that logic must never try to
// reinterpret a CCI split as a raw XISO fragment pair, named-convention or
// not. See xiso/splitting.test.ts for the detection logic itself.
describe('arbitrary-filename split detection does not misfire on CCI parts', () => {
	it('resolveArbitraryXisoSplit ignores files that detect as cci, even under arbitrary names', async () => {
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

	it('resolveBatchEntry only recognizes a CCI split via the ".1.cci"/".2.cci" naming convention, not arbitrary names', async () => {
		const files = {
			'arbitrary-name-one.bin': part1,
			'arbitrary-name-two.bin': part2,
		};
		const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));
		// Falls through to the single-file fallback: part 1 alone,
		// correctly still detected as 'cci' by magic, just not paired up.
		expect(resolved.kind).toBe('file');
		if (resolved.kind !== 'file') throw new Error('unreachable');
		expect(resolved.format).toBe('cci');
	});
});

// Format-specific splits: naming-convention detection for CCI's
// ".1.cci"/".2.cci" pair (formats::iso::resolve_batch_entry's
// detect_named_split, via find_named_split - see split_detect.rs). Unlike
// the arbitrary-filename tests above (which confirm the raw-XISO detector
// correctly leaves CCI alone), these exercise the CCI-specific detector
// itself - and unlike CISO, each CCI part is fully self-contained (own
// header, own index), so a broken pairing here can reuse the same
// "corrupt part 2's magic byte" technique already proven to fail
// CciSource::open() above.
describe('resolveBatchEntry ".1."/".2." named-split detection (CCI)', () => {
	it('reports Invalid for a named pair where part 2 has a bad magic', async () => {
		const corrupt = part2.slice();
		corrupt[0] = 0x00;
		const files = {
			'game.1.cci': part1,
			'game.2.cci': corrupt,
		};
		const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));
		expect(resolved.kind).toBe('invalid');
		if (resolved.kind !== 'invalid') throw new Error('unreachable');
		expect(resolved.names.slice().sort()).toEqual(['game.1.cci', 'game.2.cci']);
		expect(resolved.reason).toMatch(/don't form a valid Cci split/i);
		expect(resolved.reason).toMatch(/bad magic/i);
	});

	it('a lone "game.1.cci" without its "game.2.cci" pair falls back to plain single-file detection, not Invalid', async () => {
		// find_named_split requires both names to be present before
		// detect_named_split ever calls CciSource::open - a missing sibling
		// is not the same failure mode as a present-but-corrupted one, and
		// must not be reported as Invalid.
		const files = { 'game.1.cci': part1 };
		const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));
		expect(resolved.kind).toBe('file');
		if (resolved.kind !== 'file') throw new Error('unreachable');
		expect(resolved.format).toBe('cci');
	});

	// Real archivers have shipped bugs exactly here (7-Zip #2208:
	// extracting starting from a non-first volume silently misbehaves
	// instead of finding the pair) - `find_named_split` is written to
	// check both the ".1." and ".2." suffix branches regardless of which
	// half `entries[0]` is, so this pins that down as a real guarantee
	// rather than an incidental side effect of every existing test
	// happening to list part 1 first.
	it('recognizes the same named pair whether entries[0] is part 1 or part 2', async () => {
		const files = {
			'game.1.cci': part1,
			'game.2.cci': part2,
		};

		const fromPart1 = await resolveBatchEntry(
			['game.1.cci', 'game.2.cci'],
			nameMap(files),
		);
		const fromPart2 = await resolveBatchEntry(
			['game.2.cci', 'game.1.cci'],
			nameMap(files),
		);

		for (const resolved of [fromPart1, fromPart2]) {
			expect(resolved.kind).toBe('dir');
			if (resolved.kind !== 'dir') throw new Error('unreachable');
			expect(resolved.format).toBe('cci');
			expect(resolved.parts.map((p) => p.name).sort()).toEqual(
				['game.1.cci', 'game.2.cci'].sort(),
			);
		}
	});
});

// Actually crossing FILE_SPLIT_POINT (~4.28 GB) on the *write* side can't be
// exercised end-to-end here: it would require a fixture whose compressed
// output exceeds ~4.28 GB, impractical to generate or compress in a unit
// test. That side's split arithmetic is covered directly in Rust unit tests
// in cci.rs's `tests` module instead, against synthetic sizes - the same
// approach ciso.rs and xiso.rs's `split_tests` modules use for their own
// (differently-shaped) formats.
//
// The *read* side doesn't have that limitation and is covered for real just
// above: cutting a small real single-part conversion into two self-contained
// parts exercises the genuine multi-file reader path end-to-end, the same
// way ciso's split-byte-patching does for its own (differently-shaped)
// index format.
describe('ConversionSession(cci) splitting - coverage notes', () => {
	it('cciFileSplitPoint() matches the documented ~4.28 GB threshold', () => {
		expect(FILE_SPLIT_POINT).toBe(0xff000000);
	});
});
