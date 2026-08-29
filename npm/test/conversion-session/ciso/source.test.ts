import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import {
	makeReadFn,
	nullReadFn,
	throwingReadFn,
} from '../../utils/read-fns.js';
import {
	convertXisoFixtureToBytes,
	driveAndDrain,
	driveHashing,
} from '../../utils/session-helpers.js';
import {
	ConversionSession,
	cisoSectorSize,
	detectFormat,
} from '../../../dist/index.js';
import { CISO_SOURCE } from '../../utils/sources.js';

let SECTOR_SIZE: number;
let xiso: Uint8Array;
let cisoBytes: Uint8Array;

const CISO_OUTPUT_NAME = 'game';

beforeAll(async () => {
	await setupWasm();
	SECTOR_SIZE = cisoSectorSize();
	xiso = makeFixture({ titleId: 0x41560001, version: 1 });
	cisoBytes = convertXisoFixtureToBytes(xiso, {
		format: 'ciso',
		outputName: CISO_OUTPUT_NAME,
	});
});

describe('detectFormat resolves a ciso-produced container as ciso', () => {
	it('identifies the CISO magic bytes regardless of how the container was produced', () => {
		expect(detectFormat(makeReadFn(cisoBytes), cisoBytes.length)).toBe('ciso');
	});
});

describe('ConversionSession from a ciso source - error paths', () => {
	// Opening a ciso source parses the CSO header/index up front (the same
	// way opening a ciso *target* detects the XDVDFS root up front) - so
	// content that isn't a real CISO container should fail at open(), not
	// get deferred to the first hashNextPart()/nextChunk() call.
	it('throws at open() for zeroed (all-null) content declared as a ciso source', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				SECTOR_SIZE * 4,
				{ format: 'xiso' },
				CISO_SOURCE,
			),
		).toThrow();
	});
	it('throws at open() for a zero-byte input declared as a ciso source', () => {
		expect(() =>
			ConversionSession.open(nullReadFn, 0, { format: 'xiso' }, CISO_SOURCE),
		).toThrow();
	});
	it('propagates errors thrown inside readFn while opening a ciso source', () => {
		expect(() =>
			ConversionSession.open(
				throwingReadFn,
				cisoBytes.length,
				{ format: 'xiso' },
				CISO_SOURCE,
			),
		).toThrow('read error from JS');
	});
	// Same contract every other format's error-path suite locks in: `source`
	// is required, not inferred, even when the target is unambiguous.
	it('throws when source is omitted, instead of silently assuming xiso', () => {
		expect(() =>
			// @ts-expect-error - source is intentionally omitted to assert
			// the runtime contract for callers not using TS.
			ConversionSession.open(makeReadFn(cisoBytes), cisoBytes.length, {
				format: 'xiso',
			}),
		).toThrow(/source format must be resolved/);
	});
});

describe('ConversionSession(ciso source) \u2192 xiso', () => {
	it('opens without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'xiso' },
			CISO_SOURCE,
		);
		session.free();
	});
	it('reports a positive integer totalUnits', () => {
		const session = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'xiso' },
			CISO_SOURCE,
		);
		expect(session.totalUnits()).toBeGreaterThan(0);
		expect(Number.isInteger(session.totalUnits())).toBe(true);
		session.free();
	});
	// The strongest correctness check available here: since ciso (full
	// mode) is a lossless repack of the same XDVDFS content as the
	// original xiso fixture, converting *back* to xiso from the ciso
	// container should reproduce the exact same bytes as converting the
	// original fixture straight to xiso - proving nothing was lost or
	// altered on the way through the ciso container.
	it('produces byte-identical output to converting the original xiso fixture directly', () => {
		const fromCiso = driveAndDrain(
			ConversionSession.open(
				makeReadFn(cisoBytes),
				cisoBytes.length,
				{ format: 'xiso' },
				CISO_SOURCE,
			),
			64 * SECTOR_SIZE,
		);
		const fromOriginal = driveAndDrain(
			ConversionSession.open(
				makeReadFn(xiso),
				xiso.length,
				{ format: 'xiso' },
				{ source: { format: 'xiso' as const } },
			),
			64 * SECTOR_SIZE,
		);
		expect(fromCiso).toEqual(fromOriginal);
	});
	it('is deterministic across separate sessions', () => {
		const a = driveAndDrain(
			ConversionSession.open(
				makeReadFn(cisoBytes),
				cisoBytes.length,
				{ format: 'xiso' },
				CISO_SOURCE,
			),
			1,
		);
		const b = driveAndDrain(
			ConversionSession.open(
				makeReadFn(cisoBytes),
				cisoBytes.length,
				{ format: 'xiso' },
				CISO_SOURCE,
			),
			32 * SECTOR_SIZE,
		);
		expect(a).toEqual(b);
	});
});

describe('ConversionSession(ciso source) \u2192 god', () => {
	it('opens without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'god' },
			CISO_SOURCE,
		);
		session.free();
	});
	it('reports a positive integer totalUnits', () => {
		const session = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'god' },
			CISO_SOURCE,
		);
		driveHashing(session);
		expect(session.totalUnits()).toBeGreaterThan(0);
		expect(Number.isInteger(session.totalUnits())).toBe(true);
		session.free();
	});
	it('is deterministic across separate sessions', () => {
		const a = driveAndDrain(
			ConversionSession.open(
				makeReadFn(cisoBytes),
				cisoBytes.length,
				{ format: 'god' },
				CISO_SOURCE,
			),
			1,
		);
		const b = driveAndDrain(
			ConversionSession.open(
				makeReadFn(cisoBytes),
				cisoBytes.length,
				{ format: 'god' },
				CISO_SOURCE,
			),
			32 * SECTOR_SIZE,
		);
		expect(a).toEqual(b);
	});
});

describe('ConversionSession(ciso source) \u2192 cci', () => {
	const CCI_OUTPUT_NAME = 'test';
	it('opens without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'cci', outputName: CCI_OUTPUT_NAME },
			CISO_SOURCE,
		);
		driveHashing(session);
		session.free();
	});
	it('totalUnits matches the xiso-sourced baseline', () => {
		const fromCiso = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'cci', outputName: CCI_OUTPUT_NAME },
			CISO_SOURCE,
		);
		driveHashing(fromCiso);
		const cisoUnits = fromCiso.totalUnits();
		fromCiso.free();

		const fromOriginal = ConversionSession.open(
			makeReadFn(xiso),
			xiso.length,
			{ format: 'cci', outputName: CCI_OUTPUT_NAME },
			{ source: { format: 'xiso' as const } },
		);
		driveHashing(fromOriginal);
		const originalUnits = fromOriginal.totalUnits();
		fromOriginal.free();

		expect(cisoUnits).toBeGreaterThan(0);
		expect(cisoUnits).toBe(originalUnits);
	});
	// Cross-format sanity check: the CCI header (magic + uncompressed_size)
	// should describe the same underlying content whether cci was reached
	// via the original fixture or via the ciso container.
	it('produces a CCI header whose magic and uncompressed_size match the xiso-sourced baseline', () => {
		const session = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'cci', outputName: CCI_OUTPUT_NAME },
			CISO_SOURCE,
		);
		driveHashing(session);
		const totalUnits = session.totalUnits();
		const header = session.nextChunk(32)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		const magic = String.fromCharCode(
			view.getUint8(0),
			view.getUint8(1),
			view.getUint8(2),
			view.getUint8(3),
		);
		expect(magic).toBe('CCIM');
		expect(view.getBigUint64(8, true)).toBe(BigInt(totalUnits * SECTOR_SIZE));
	});
	it('is deterministic across separate sessions', () => {
		const a = driveAndDrain(
			ConversionSession.open(
				makeReadFn(cisoBytes),
				cisoBytes.length,
				{ format: 'cci', outputName: CCI_OUTPUT_NAME },
				CISO_SOURCE,
			),
			1,
		);
		const b = driveAndDrain(
			ConversionSession.open(
				makeReadFn(cisoBytes),
				cisoBytes.length,
				{ format: 'cci', outputName: CCI_OUTPUT_NAME },
				CISO_SOURCE,
			),
			32 * SECTOR_SIZE,
		);
		expect(a).toEqual(b);
	});
});

describe('ConversionSession(ciso source) \u2192 extracted', () => {
	it('opens without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'extracted' },
			CISO_SOURCE,
		);
		driveHashing(session);
		session.free();
	});
	it('reports a non-empty outputManifest once sizing completes', () => {
		const session = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'extracted' },
			CISO_SOURCE,
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest.length).toBeGreaterThan(0);
	});
	// The set of extracted file names shouldn't depend on whether the
	// session was sourced from the original xiso fixture or from a ciso
	// repack of the exact same content.
	it('extracts the same file names as extracting the original xiso fixture directly', () => {
		const fromCiso = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'extracted' },
			CISO_SOURCE,
		);
		driveHashing(fromCiso);
		const cisoNames = fromCiso
			.outputManifest()
			.map((entry) => entry.name)
			.sort();
		fromCiso.free();

		const fromOriginal = ConversionSession.open(
			makeReadFn(xiso),
			xiso.length,
			{ format: 'extracted' },
			{ source: { format: 'xiso' as const } },
		);
		driveHashing(fromOriginal);
		const originalNames = fromOriginal
			.outputManifest()
			.map((entry) => entry.name)
			.sort();
		fromOriginal.free();

		expect(cisoNames).toEqual(originalNames);
	});
});

describe('ConversionSession(ciso source) \u2192 zar', () => {
	const ZAR_OUTPUT_NAME = 'test';
	it('opens without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'zar', outputName: ZAR_OUTPUT_NAME },
			CISO_SOURCE,
		);
		session.free();
	});
	it('reports a positive integer totalUnits', () => {
		const session = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'zar', outputName: ZAR_OUTPUT_NAME },
			CISO_SOURCE,
		);
		expect(session.totalUnits()).toBeGreaterThan(0);
		expect(Number.isInteger(session.totalUnits())).toBe(true);
		session.free();
	});
	it('is deterministic across separate sessions', () => {
		const a = driveAndDrain(
			ConversionSession.open(
				makeReadFn(cisoBytes),
				cisoBytes.length,
				{ format: 'zar', outputName: ZAR_OUTPUT_NAME },
				CISO_SOURCE,
			),
			1,
		);
		const b = driveAndDrain(
			ConversionSession.open(
				makeReadFn(cisoBytes),
				cisoBytes.length,
				{ format: 'zar', outputName: ZAR_OUTPUT_NAME },
				CISO_SOURCE,
			),
			32 * SECTOR_SIZE,
		);
		expect(a).toEqual(b);
	});
});
