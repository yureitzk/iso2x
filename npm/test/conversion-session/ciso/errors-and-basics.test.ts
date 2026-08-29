import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import {
	makeReadFn,
	nullReadFn,
	throwingReadFn,
} from '../../utils/read-fns.js';
import {
	driveHashing,
	drain,
	driveAndDrain,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import {
	ConversionSession,
	cisoFilePaddingModulus,
	cisoSectorSize,
} from '../../../dist/index.js';
import { XISO_SOURCE } from '../../utils/sources.js';

let SECTOR_SIZE: number;
let FILE_PADDING_MODULUS: number;

beforeAll(async () => {
	await setupWasm();
	SECTOR_SIZE = cisoSectorSize();
	FILE_PADDING_MODULUS = cisoFilePaddingModulus();
});

// outputName is required for ciso - split file names are derived from it
// ("<outputName>.1.cso", "<outputName>.2.cso", ...).
const OUTPUT_NAME = 'test';

describe('ConversionSession(ciso) error paths', () => {
	it('throws when the input size is not a multiple of the 2048-byte sector size', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				SECTOR_SIZE * 4 + 1,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
		).toThrow();
	});

	// ciso builds its output from a *repacked* XDVDFS image (same root-offset
	// detection + create_xdvdfs_image pipeline as xiso), not a raw sector
	// copy of the upload. Root-offset detection happens synchronously inside
	// open(), so content that isn't a parseable XDVDFS filesystem throws
	// there - this is not "sector alignment only" like xiso's raw-copy path.
	it('throws at open() for zeroed (all-null) content - ciso requires a parseable XDVDFS filesystem, not just sector alignment', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				SECTOR_SIZE * 4,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
		).toThrow();
	});

	// Root-offset detection reads via readFn as the very first step of
	// open(), so a readFn that throws surfaces the error at open() itself -
	// not deferred to the first hashNextPart() call.
	it('propagates errors thrown inside readFn - root-offset detection reads at open() time', () => {
		expect(() =>
			ConversionSession.open(
				throwingReadFn,
				SECTOR_SIZE * 4,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
		).toThrow();
	});

	// A zero-byte input has no filesystem to detect a root offset from, so
	// this fails the same way the zeroed-content case above does.
	it('throws at open() for a zero-byte input - no filesystem to detect a root offset from', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
		).toThrow();
	});

	// `source` is required, not optional - fails loudly rather than
	// silently defaulting, so nothing can skip the resolve step.
	it('throws when source is omitted, instead of silently assuming xiso', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const readFn = makeReadFn(iso);
		expect(() =>
			// @ts-expect-error - source is intentionally omitted to assert
			// the runtime contract for callers not using TS.
			ConversionSession.open(readFn, iso.length, {
				format: 'ciso',
				outputName: OUTPUT_NAME,
			}),
		).toThrow(/source format must be resolved/);
	});
});

describe('ConversionSession(ciso) with minimal fixture', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);
	// ciso streams sectors of the *repacked* XDVDFS image, not a raw copy of
	// the upload - so totalUnits() has no fixed relationship to
	// iso.length / SECTOR_SIZE (repacking can grow the image to satisfy
	// XDVDFS layout/alignment requirements just as easily as it can shrink
	// it by stripping padding). xiso repacks the same input through the
	// same create_xdvdfs_image pipeline, so its totalUnits() (sectors) is
	// an independent ground truth to compare against.
	it('totalUnits reflects the repacked image size, matching xiso\u2019s sector count for the same input', () => {
		const cisoSession = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(cisoSession);
		const cisoUnits = cisoSession.totalUnits();
		cisoSession.free();
		const xisoSession = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'xiso',
			},
			XISO_SOURCE,
		);
		const xisoUnits = xisoSession.totalUnits();
		xisoSession.free();
		expect(cisoUnits).toBeGreaterThan(0);
		expect(Number.isInteger(cisoUnits)).toBe(true);
		expect(cisoUnits).toBe(xisoUnits);
	});

	it('totalUnits is a positive integer', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		expect(session.totalUnits()).toBeGreaterThan(0);
		expect(Number.isInteger(session.totalUnits())).toBe(true);
		session.free();
	});

	it('is not done immediately, even after hashNextPart() completes', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		expect(session.isDone()).toBe(false);
		session.free();
	});

	// ciso always reports an entry name, the same way extracted does; for
	// output that stays under the split threshold (as this tiny fixture
	// always will) it collapses to the bare "<outputName>.cso" name for
	// every chunk, same as output_name_for's single-part case.
	it('currentEntryName is "<outputName>.cso" for every chunk, for output that never crosses the split threshold', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		let chunkCount = 0;
		while (!session.isDone()) {
			session.nextChunk(SECTOR_SIZE);
			expect(session.currentEntryName()).toBe(`${OUTPUT_NAME}.cso`);
			chunkCount++;
		}
		session.free();
		expect(chunkCount).toBeGreaterThan(0);
	});

	// ciso reports a manifest only once sizing (hashNextPart) has
	// completed - unlike god/extracted, exact split sizes depend on
	// per-sector compression ratios that aren't known any earlier.
	it('outputManifest is empty before hashNextPart() completes', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		expect(session.outputManifest()).toEqual([]);
		session.free();
	});

	it('outputManifest has exactly one entry for output under the split threshold, matching the drained byte count', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		const out = drain(session, 64 * SECTOR_SIZE);
		expect(manifest).toHaveLength(1);
		expect(manifest[0].name).toBe(`${OUTPUT_NAME}.cso`);
		expect(manifest[0].size).toBe(out.length);
		expect(manifest[0].size % FILE_PADDING_MODULUS).toBe(0);
	});

	it('outputManifest entry names are derived from outputName', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: 'Halo 3',
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest[0].name).toBe('Halo 3.cso');
	});

	it('nextChunk returns null once all output is consumed', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		expect(session.nextChunk(1)).toBeNull();
		session.free();
	});

	it('chunk size does not affect final output (chunking is transport-only)', () => {
		const out1 = driveAndDrain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
			1,
		);
		const out32 = driveAndDrain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
			32 * SECTOR_SIZE,
		);
		const outAll = driveAndDrain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		expect(out1).toEqual(out32);
		expect(out32).toEqual(outAll);
	});

	it('is deterministic across separate sessions', () => {
		const a = driveAndDrain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
			64 * SECTOR_SIZE,
		);
		const b = driveAndDrain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
			64 * SECTOR_SIZE,
		);
		expect(a).toEqual(b);
	});
});
