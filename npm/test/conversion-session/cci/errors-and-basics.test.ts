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
import { ConversionSession, cciSectorSize } from '../../../dist/index.js';
import { XISO_SOURCE } from '../../utils/sources.js';

// CCI doesn't expose its own sector-size getter - cci.rs imports
// `SECTOR_SIZE` directly from `crate::ciso` rather than redefining it (see
// the `use crate::ciso::SECTOR_SIZE;` at the top of cci.rs), so the two
// formats share the exact same constant. Reusing `cisoSectorSize()` here
// (rather than hardcoding 2048) means this suite stays correct if that
// shared constant ever changes.
let SECTOR_SIZE: number;
beforeAll(async () => {
	await setupWasm();
	SECTOR_SIZE = cciSectorSize();
});

// outputName is required for cci - split file names are derived from it
// ("<outputName>.1.cci", "<outputName>.2.cci", ...).
const OUTPUT_NAME = 'test';

describe('ConversionSession(cci) error paths', () => {
	it('throws when the input size is not a multiple of the 2048-byte sector size', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				SECTOR_SIZE * 4 + 1,
				{
					format: 'cci',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
		).toThrow();
	});
	// cci builds its output from a *repacked* XDVDFS image (same root-offset
	// detection + create_xdvdfs_image pipeline as ciso/xiso), not a raw
	// sector copy of the upload. Root-offset detection happens synchronously
	// inside open(), so content that isn't a parseable XDVDFS filesystem
	// throws there.
	it('throws at open() for zeroed (all-null) content - cci requires a parseable XDVDFS filesystem, not just sector alignment', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				SECTOR_SIZE * 4,
				{
					format: 'cci',
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
					format: 'cci',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
		).toThrow();
	});
	it('throws at open() for a zero-byte input - no filesystem to detect a root offset from', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{
					format: 'cci',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
		).toThrow();
	});
	// `source` is required - omitting it must fail loudly rather than
	// silently assuming xiso, so nothing can skip the resolve step.
	it('throws when source is omitted, instead of silently assuming xiso', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const readFn = makeReadFn(iso);
		expect(() =>
			// @ts-expect-error - source is intentionally omitted to assert
			// the runtime contract for callers not using TS.
			ConversionSession.open(readFn, iso.length, {
				format: 'cci',
				outputName: OUTPUT_NAME,
			}),
		).toThrow(/source format must be resolved/);
	});
});

describe('ConversionSession(cci) with minimal fixture', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);
	// cci streams sectors of the *repacked* XDVDFS image, not a raw copy of
	// the upload - so totalUnits() has no fixed relationship to
	// iso.length / SECTOR_SIZE. xiso repacks the same input through the same
	// create_xdvdfs_image pipeline, so its totalUnits() (sectors) is an
	// independent ground truth to compare against.
	it('totalUnits reflects the repacked image size, matching xiso\u2019s sector count for the same input', () => {
		const cciSession = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'cci',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(cciSession);
		const cciUnits = cciSession.totalUnits();
		cciSession.free();
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
		expect(cciUnits).toBeGreaterThan(0);
		expect(Number.isInteger(cciUnits)).toBe(true);
		expect(cciUnits).toBe(xisoUnits);
	});
	it('totalUnits is a positive integer', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'cci',
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
				format: 'cci',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		expect(session.isDone()).toBe(false);
		session.free();
	});
	it('currentEntryName is "<outputName>.cci" for every chunk, for output that never crosses the split threshold', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'cci',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		let chunkCount = 0;
		while (!session.isDone()) {
			session.nextChunk(SECTOR_SIZE);
			expect(session.currentEntryName()).toBe(`${OUTPUT_NAME}.cci`);
			chunkCount++;
		}
		session.free();
		expect(chunkCount).toBeGreaterThan(0);
	});
	it('outputManifest is empty before hashNextPart() completes', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'cci',
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
				format: 'cci',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		const out = drain(session, 64 * SECTOR_SIZE);
		expect(manifest).toHaveLength(1);
		expect(manifest[0].name).toBe(`${OUTPUT_NAME}.cci`);
		expect(manifest[0].size).toBe(out.length);
	});
	it('outputManifest entry names are derived from outputName', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'cci',
				outputName: 'Halo 3',
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest[0].name).toBe('Halo 3.cci');
	});
	it('nextChunk returns null once all output is consumed', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'cci',
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
					format: 'cci',
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
					format: 'cci',
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
					format: 'cci',
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
					format: 'cci',
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
					format: 'cci',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
			64 * SECTOR_SIZE,
		);
		expect(a).toEqual(b);
	});
});
