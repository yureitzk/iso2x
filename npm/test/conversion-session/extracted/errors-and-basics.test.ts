import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import {
	makeFixture,
	DEFAULT_XBE_DECLARED_SIZE,
} from '../../utils/fixtures/xsf.js';
import { ConversionSession, detectFormat } from '../../../dist/index.js';
import {
	makeReadFn,
	nullReadFn,
	throwingReadFn,
} from '../../utils/read-fns.js';
import {
	drain,
	convertXisoFixtureToBytes,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import { XISO_SOURCE, ZAR_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

function isSystemUpdateEntry(name: string): boolean {
	return name
		.replace(/^[/\\]+/, '')
		.toUpperCase()
		.startsWith('$SYSTEMUPDATE');
}

describe('ConversionSession(extracted) error paths', () => {
	it('throws for a zeroed (invalid) image', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				10 * 1024 * 1024,
				{
					format: 'extracted',
				},
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('propagates errors thrown inside readFn', () => {
		expect(() =>
			ConversionSession.open(
				throwingReadFn,
				10 * 1024 * 1024,
				{
					format: 'extracted',
				},
				XISO_SOURCE,
			),
		).toThrow('read error from JS');
	});

	it('throws when source is omitted, instead of silently assuming xiso', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const readFn = makeReadFn(iso);
		expect(() =>
			// @ts-expect-error - source is intentionally omitted to assert
			// the runtime contract for callers not using TS.
			ConversionSession.open(readFn, iso.length, { format: 'extracted' }),
		).toThrow(/source format must be resolved/);
	});
});

describe('ConversionSession(extracted) with minimal fixture (single file)', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);

	it('detectFormat resolves this fixture as xiso (the source shape extracted conversion consumes)', () => {
		expect(detectFormat(readFn, iso.length)).toBe('xiso');
	});

	it('opens without throwing', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
			},
			XISO_SOURCE,
		);
		session.free();
	});

	it('totalUnits equals the file count (1 for the fixture)', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
			},
			XISO_SOURCE,
		);
		expect(session.totalUnits()).toBe(1);
		session.free();
	});

	it('currentEntryName is null before the first nextChunk call', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
			},
			XISO_SOURCE,
		);
		expect(session.currentEntryName()).toBeNull();
		session.free();
	});

	it('currentEntryName reports default.xbe after the first chunk', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
			},
			XISO_SOURCE,
		);
		session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		expect(session.currentEntryName()).toBe('default.xbe');
		session.free();
	});

	it('drains exactly one chunk for a single-file image, then reports done', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
			},
			XISO_SOURCE,
		);
		const first = session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		expect(first).toBeInstanceOf(Uint8Array);
		expect(session.isDone()).toBe(true);
		expect(session.nextChunk(UNBOUNDED_CHUNK_SIZE)).toBeNull();
		session.free();
	});

	it('extracted bytes match the source file bytes at the recorded offset', () => {
		// default.xbe starts at sector 0x22 in the fixture; the directory
		// entry declares its size as DEFAULT_XBE_DECLARED_SIZE (see xsf.ts).
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
			},
			XISO_SOURCE,
		);
		const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		session.free();
		const expectedOffset = 0x22 * 0x800;
		const expectedSize = DEFAULT_XBE_DECLARED_SIZE;
		expect(chunk.length).toBe(expectedSize);
		expect(chunk).toEqual(
			iso.slice(expectedOffset, expectedOffset + expectedSize),
		);
	});
});

describe('ConversionSession(extracted) outputManifest', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);
	it('is available immediately after open, before any nextChunk call', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
			},
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest).toEqual([
			{ name: 'default.xbe', size: DEFAULT_XBE_DECLARED_SIZE },
		]);
	});

	it('has one entry per file (matches totalUnits)', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
			},
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		const totalUnits = session.totalUnits();
		session.free();
		expect(manifest.length).toBe(totalUnits);
	});

	it('manifest sizes sum to the same total predictLength() would use', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
			},
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		const total = manifest.reduce((sum, e) => sum + e.size, 0);
		expect(total).toBe(DEFAULT_XBE_DECLARED_SIZE);
	});
});

describe('ConversionSession(extracted) skipSystemUpdate option', () => {
	const iso = makeFixture({ titleId: 0x41560001, includeSystemUpdate: true });
	const readFn = makeReadFn(iso);

	it('includes the $SystemUpdate file when skipSystemUpdate is unset (default)', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
			},
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest.length).toBe(2);
		expect(manifest.some((e) => e.name === 'default.xbe')).toBe(true);
		expect(manifest.some((e) => isSystemUpdateEntry(e.name))).toBe(true);
	});

	it('includes the $SystemUpdate file when skipSystemUpdate is explicitly false', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
				skipSystemUpdate: false,
			},
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest.length).toBe(2);
		expect(manifest.some((e) => isSystemUpdateEntry(e.name))).toBe(true);
	});

	it('excludes the $SystemUpdate file when skipSystemUpdate is true', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
				skipSystemUpdate: true,
			},
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest).toEqual([
			{ name: 'default.xbe', size: DEFAULT_XBE_DECLARED_SIZE },
		]);
		expect(manifest.some((e) => isSystemUpdateEntry(e.name))).toBe(false);
	});

	it('excludes the system update directory when skipSystemUpdate is true, even if its on-disk name is uppercase', () => {
		const upperIso = makeFixture({
			titleId: 0x41560001,
			includeSystemUpdate: true,
			systemUpdateDirName: '$SYSTEMUPDATE',
		});
		const session = ConversionSession.open(
			makeReadFn(upperIso),
			upperIso.length,
			{
				format: 'extracted',
				skipSystemUpdate: true,
			},
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest).toEqual([
			{ name: 'default.xbe', size: DEFAULT_XBE_DECLARED_SIZE },
		]);
		expect(manifest.some((e) => isSystemUpdateEntry(e.name))).toBe(false);
	});

	it('totalUnits reflects the filtered file count when skipSystemUpdate is true', () => {
		const withUpdate = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
				skipSystemUpdate: false,
			},
			XISO_SOURCE,
		);
		expect(withUpdate.totalUnits()).toBe(2);
		withUpdate.free();
		const skipped = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
				skipSystemUpdate: true,
			},
			XISO_SOURCE,
		);
		expect(skipped.totalUnits()).toBe(1);
		skipped.free();
	});

	it('never surfaces a $SystemUpdate entry via nextChunk when skipSystemUpdate is true', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'extracted',
				skipSystemUpdate: true,
			},
			XISO_SOURCE,
		);
		const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		expect(chunk).toBeInstanceOf(Uint8Array);
		expect(session.currentEntryName()).toBe('default.xbe');
		expect(session.isDone()).toBe(true);
		expect(session.nextChunk(UNBOUNDED_CHUNK_SIZE)).toBeNull();
		session.free();
	});

	it('does not affect fixtures with no $SystemUpdate directory at all', () => {
		const plainIso = makeFixture({ titleId: 0x41560001 });
		const plainReadFn = makeReadFn(plainIso);
		const session = ConversionSession.open(
			plainReadFn,
			plainIso.length,
			{
				format: 'extracted',
				skipSystemUpdate: true,
			},
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest).toEqual([
			{ name: 'default.xbe', size: DEFAULT_XBE_DECLARED_SIZE },
		]);
	});
});

describe('ConversionSession(extracted) from a zar source', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	let zarBytes: Uint8Array;

	beforeAll(() => {
		zarBytes = convertXisoFixtureToBytes(iso, {
			format: 'zar',
			outputName: 'game',
		});
	});

	it('opens and drains without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(zarBytes),
			zarBytes.length,
			{ format: 'extracted' },
			ZAR_SOURCE,
		);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	it('totalUnits equals the file count (1 for the fixture)', () => {
		const session = ConversionSession.open(
			makeReadFn(zarBytes),
			zarBytes.length,
			{ format: 'extracted' },
			ZAR_SOURCE,
		);
		expect(session.totalUnits()).toBe(1);
		session.free();
	});

	it('currentEntryName reports default.xbe after the first chunk', () => {
		const session = ConversionSession.open(
			makeReadFn(zarBytes),
			zarBytes.length,
			{ format: 'extracted' },
			ZAR_SOURCE,
		);
		session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		expect(session.currentEntryName()).toBe('default.xbe');
		session.free();
	});

	it('outputManifest matches the packed content (default.xbe, declared size)', () => {
		const session = ConversionSession.open(
			makeReadFn(zarBytes),
			zarBytes.length,
			{ format: 'extracted' },
			ZAR_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest).toEqual([
			{ name: 'default.xbe', size: DEFAULT_XBE_DECLARED_SIZE },
		]);
	});

	it('extracted bytes match the source file bytes at the recorded offset', () => {
		const session = ConversionSession.open(
			makeReadFn(zarBytes),
			zarBytes.length,
			{ format: 'extracted' },
			ZAR_SOURCE,
		);
		const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		session.free();
		const expectedOffset = 0x22 * 0x800;
		const expectedSize = DEFAULT_XBE_DECLARED_SIZE;
		expect(chunk.length).toBe(expectedSize);
		expect(chunk).toEqual(
			iso.slice(expectedOffset, expectedOffset + expectedSize),
		);
	});

	it('produces byte-identical output to unpacking the same content straight from the xiso image', () => {
		const fromZar = drain(
			ConversionSession.open(
				makeReadFn(zarBytes),
				zarBytes.length,
				{ format: 'extracted' },
				ZAR_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		const fromImage = drain(
			ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'extracted' },
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		expect(fromZar).toEqual(fromImage);
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				zarBytes.length,
				{ format: 'extracted' },
				{ source: ZAR_SOURCE.source, parts: [] },
			),
		).toThrow();
	});
});
