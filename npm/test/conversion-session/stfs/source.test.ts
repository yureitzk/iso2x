import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeStfsFixture } from '../../utils/fixtures/stfs.js';
import {
	ConversionSession,
	cisoSectorSize,
	detectFormat,
} from '../../../dist/index.js';
import {
	makeReadFn,
	nullReadFn,
	throwingReadFn,
} from '../../utils/read-fns.js';
import { driveAndDrain, driveHashing } from '../../utils/session-helpers.js';
import {
	STFS_SOURCE_OPTIONS as STFS_SOURCE,
	XISO_SOURCE_OPTIONS,
	CCI_SOURCE_OPTIONS,
} from '../../utils/sources.js';

let SECTOR_SIZE: number;
let pkg: Uint8Array;

const STFS_OUTPUT_NAME = 'game';
// The STFS fixture's one file, per stfs.ts's writeXexStub -
// declared at the file-listing entry, independent of the real XEX2 stub
// bytes written after it.
const STFS_FILE_NAME = 'default.xex';
const STFS_FILE_SIZE = 0x100;

beforeAll(async () => {
	await setupWasm();
	SECTOR_SIZE = cisoSectorSize();
	({ bytes: pkg } = makeStfsFixture({ titleId: 0x53540001, version: 1 }));
});

describe('detectFormat resolves an stfs-produced container as stfs', () => {
	it('identifies the CON magic bytes regardless of how the container was produced', () => {
		expect(detectFormat(makeReadFn(pkg), pkg.length)).toBe('stfs');
	});
});

describe('ConversionSession from an stfs source - error paths', () => {
	// Opening an stfs source parses the header/volume descriptor/file
	// listing up front (the same way opening a ciso *target* detects the
	// XDVDFS root up front) - so content that isn't a real STFS package
	// should fail at open(), not get deferred to the first
	// hashNextPart()/nextChunk() call.
	it('throws at open() for zeroed (all-null) content declared as an stfs source', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				SECTOR_SIZE * 4,
				{ format: 'xiso' },
				{ source: STFS_SOURCE },
			),
		).toThrow();
	});

	it('throws at open() for a zero-byte input declared as an stfs source', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'xiso' },
				{
					source: STFS_SOURCE,
				},
			),
		).toThrow();
	});

	it('propagates errors thrown inside readFn while opening an stfs source', () => {
		expect(() =>
			ConversionSession.open(
				throwingReadFn,
				pkg.length,
				{ format: 'xiso' },
				{ source: STFS_SOURCE },
			),
		).toThrow('read error from JS');
	});

	// Same contract every other format's error-path suite locks in: `source`
	// is required, not inferred, even when the target is unambiguous.
	it('throws when source is omitted, instead of silently assuming xiso', () => {
		expect(() =>
			// @ts-expect-error - source is intentionally omitted to assert
			// the runtime contract for callers not using TS.
			ConversionSession.open(makeReadFn(pkg), pkg.length, {
				format: 'xiso',
			}),
		).toThrow(/source format must be resolved/);
	});

	// STFS never splits across multiple parts (unlike Ciso/Cci/God) - see
	// the identical "parts.len() == 1" check in core::source::open's Stfs
	// arm, which mirrors Zar.
	it('throws when sourceParts has more than one entry', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				pkg.length,
				{ format: 'xiso' },
				{
					source: STFS_SOURCE,
					parts: [
						{ name: 'a', size: pkg.length, readFn: makeReadFn(pkg) },
						{ name: 'b', size: pkg.length, readFn: makeReadFn(pkg) },
					],
				},
			),
		).toThrow(/multiple parts/);
	});
});

describe('ConversionSession(stfs source) \u2192 xiso', () => {
	it('opens without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(pkg),
			pkg.length,
			{ format: 'xiso' },
			{ source: STFS_SOURCE },
		);
		session.free();
	});

	it('reports a positive integer totalUnits', () => {
		const session = ConversionSession.open(
			makeReadFn(pkg),
			pkg.length,
			{ format: 'xiso' },
			{ source: STFS_SOURCE },
		);
		expect(session.totalUnits()).toBeGreaterThan(0);
		expect(Number.isInteger(session.totalUnits())).toBe(true);
		session.free();
	});

	// The strongest correctness check available here without a full-tree
	// STFS fixture: repacking the package's one file into an XDVDFS image
	// and reading it back out as extracted files should reproduce the same
	// name and size the STFS fixture itself declares.
	it('round-trips through extracted with the same single file name and size the STFS fixture declares', () => {
		const xisoBytes = driveAndDrain(
			ConversionSession.open(
				makeReadFn(pkg),
				pkg.length,
				{ format: 'xiso' },
				{ source: STFS_SOURCE },
			),
			64 * SECTOR_SIZE,
		);
		const extractedSession = ConversionSession.open(
			makeReadFn(xisoBytes),
			xisoBytes.length,
			{ format: 'extracted' },
			{ source: XISO_SOURCE_OPTIONS },
		);
		const manifest = extractedSession.outputManifest();
		extractedSession.free();
		expect(manifest).toHaveLength(1);
		expect(manifest[0].name).toBe(STFS_FILE_NAME);
		expect(manifest[0].size).toBe(STFS_FILE_SIZE);
	});

	it('is deterministic across separate sessions', () => {
		const a = driveAndDrain(
			ConversionSession.open(
				makeReadFn(pkg),
				pkg.length,
				{ format: 'xiso' },
				{ source: STFS_SOURCE },
			),
			1,
		);
		const b = driveAndDrain(
			ConversionSession.open(
				makeReadFn(pkg),
				pkg.length,
				{ format: 'xiso' },
				{ source: STFS_SOURCE },
			),
			32 * SECTOR_SIZE,
		);
		expect(a).toEqual(b);
	});
});

describe('ConversionSession(stfs source) \u2192 god', () => {
	it('opens without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(pkg),
			pkg.length,
			{ format: 'god' },
			{ source: STFS_SOURCE },
		);
		driveHashing(session);
		session.free();
	});

	it('reports a positive integer totalUnits', () => {
		const session = ConversionSession.open(
			makeReadFn(pkg),
			pkg.length,
			{ format: 'god' },
			{ source: STFS_SOURCE },
		);
		driveHashing(session);
		expect(session.totalUnits()).toBeGreaterThan(0);
		expect(Number.isInteger(session.totalUnits())).toBe(true);
		session.free();
	});

	it('is deterministic across separate sessions', () => {
		const a = driveAndDrain(
			ConversionSession.open(
				makeReadFn(pkg),
				pkg.length,
				{ format: 'god' },
				{ source: STFS_SOURCE },
			),
			1,
		);
		const b = driveAndDrain(
			ConversionSession.open(
				makeReadFn(pkg),
				pkg.length,
				{ format: 'god' },
				{ source: STFS_SOURCE },
			),
			32 * SECTOR_SIZE,
		);
		expect(a).toEqual(b);
	});
});

describe('ConversionSession(stfs source) \u2192 cci', () => {
	const CCI_OUTPUT_NAME = 'test';
	it('opens without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(pkg),
			pkg.length,
			{ format: 'cci', outputName: CCI_OUTPUT_NAME },
			{ source: STFS_SOURCE },
		);
		driveHashing(session);
		session.free();
	});

	it('reports a positive integer totalUnits', () => {
		const session = ConversionSession.open(
			makeReadFn(pkg),
			pkg.length,
			{ format: 'cci', outputName: CCI_OUTPUT_NAME },
			{ source: STFS_SOURCE },
		);
		driveHashing(session);
		expect(session.totalUnits()).toBeGreaterThan(0);
		expect(Number.isInteger(session.totalUnits())).toBe(true);
		session.free();
	});

	// Same header sanity check the ciso suite runs on its own cci target:
	// the CCI magic and uncompressed_size should describe whatever content
	// actually got packed in, regardless of what fed the writer.
	it('produces a CCI header whose magic and uncompressed_size match totalUnits', () => {
		const session = ConversionSession.open(
			makeReadFn(pkg),
			pkg.length,
			{ format: 'cci', outputName: CCI_OUTPUT_NAME },
			{ source: STFS_SOURCE },
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

	// Round-trip check, same shape as the xiso target above: pack through
	// cci, then read it back out via `{ format: 'cci' }` as an extracted
	// source and confirm the one file survived unchanged.
	it('round-trips through extracted with the same single file name and size the STFS fixture declares', () => {
		const cciBytes = driveAndDrain(
			ConversionSession.open(
				makeReadFn(pkg),
				pkg.length,
				{ format: 'cci', outputName: CCI_OUTPUT_NAME },
				{ source: STFS_SOURCE },
			),
			64 * SECTOR_SIZE,
		);
		const extractedSession = ConversionSession.open(
			makeReadFn(cciBytes),
			cciBytes.length,
			{ format: 'extracted' },
			{ source: CCI_SOURCE_OPTIONS },
		);
		driveHashing(extractedSession);
		const manifest = extractedSession.outputManifest();
		extractedSession.free();
		expect(manifest).toHaveLength(1);
		expect(manifest[0].name).toBe(STFS_FILE_NAME);
		expect(manifest[0].size).toBe(STFS_FILE_SIZE);
	});

	it('is deterministic across separate sessions', () => {
		const a = driveAndDrain(
			ConversionSession.open(
				makeReadFn(pkg),
				pkg.length,
				{ format: 'cci', outputName: CCI_OUTPUT_NAME },
				{ source: STFS_SOURCE },
			),
			1,
		);
		const b = driveAndDrain(
			ConversionSession.open(
				makeReadFn(pkg),
				pkg.length,
				{ format: 'cci', outputName: CCI_OUTPUT_NAME },
				{ source: STFS_SOURCE },
			),
			32 * SECTOR_SIZE,
		);
		expect(a).toEqual(b);
	});
});

describe('ConversionSession(stfs source) \u2192 extracted', () => {
	it('opens without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(pkg),
			pkg.length,
			{ format: 'extracted' },
			{ source: STFS_SOURCE },
		);
		session.free();
	});

	it('reports a non-empty outputManifest', () => {
		const session = ConversionSession.open(
			makeReadFn(pkg),
			pkg.length,
			{ format: 'extracted' },
			{ source: STFS_SOURCE },
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest.length).toBeGreaterThan(0);
	});

	// The most direct check available for this source: the STFS package's
	// one file-listing entry should come through extraction with exactly
	// the name/size the fixture wrote into that entry.
	it('extracts a single default.xex entry matching the STFS fixture\u2019s declared name and size', () => {
		const session = ConversionSession.open(
			makeReadFn(pkg),
			pkg.length,
			{ format: 'extracted' },
			{ source: STFS_SOURCE },
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest).toHaveLength(1);
		expect(manifest[0].name).toBe(STFS_FILE_NAME);
		expect(manifest[0].size).toBe(STFS_FILE_SIZE);
	});
});

describe('ConversionSession(stfs source) \u2192 zar', () => {
	it('opens without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(pkg),
			pkg.length,
			{ format: 'zar', outputName: STFS_OUTPUT_NAME },
			{ source: STFS_SOURCE },
		);
		session.free();
	});

	it('reports a positive integer totalUnits', () => {
		const session = ConversionSession.open(
			makeReadFn(pkg),
			pkg.length,
			{ format: 'zar', outputName: STFS_OUTPUT_NAME },
			{ source: STFS_SOURCE },
		);
		expect(session.totalUnits()).toBeGreaterThan(0);
		expect(Number.isInteger(session.totalUnits())).toBe(true);
		session.free();
	});

	it('is deterministic across separate sessions', () => {
		const a = driveAndDrain(
			ConversionSession.open(
				makeReadFn(pkg),
				pkg.length,
				{ format: 'zar', outputName: STFS_OUTPUT_NAME },
				{ source: STFS_SOURCE },
			),
			1,
		);
		const b = driveAndDrain(
			ConversionSession.open(
				makeReadFn(pkg),
				pkg.length,
				{ format: 'zar', outputName: STFS_OUTPUT_NAME },
				{ source: STFS_SOURCE },
			),
			32 * SECTOR_SIZE,
		);
		expect(a).toEqual(b);
	});
});
