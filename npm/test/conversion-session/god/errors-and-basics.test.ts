import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { ConversionSession, detectFormat } from '../../../dist/index.js';
import {
	makeReadFn,
	nullReadFn,
	throwingReadFn,
} from '../../utils/read-fns.js';
import {
	driveHashing,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

describe('ConversionSession(god) error paths', () => {
	it('throws for a zeroed (invalid) image', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				10 * 1024 * 1024,
				{ format: 'god' },
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('propagates errors thrown inside readFn', () => {
		expect(() =>
			ConversionSession.open(
				throwingReadFn,
				10 * 1024 * 1024,
				{ format: 'god' },
				XISO_SOURCE,
			),
		).toThrow('read error from JS');
	});

	it('throws for a zero file size', () => {
		expect(() =>
			ConversionSession.open(nullReadFn, 0, { format: 'god' }, XISO_SOURCE),
		).toThrow();
	});

	it('throws for an invalid mode value', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				10 * 1024 * 1024,
				{
					format: 'god',
					// @ts-expect-error deliberately invalid to exercise the serde error path
					mode: 'bogus',
				},
				XISO_SOURCE,
			),
		).toThrow();
	});

	// `source` is required - nothing should be able to skip the resolve step
	// by omitting it.
	it('throws when source is omitted, instead of silently assuming xiso', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const readFn = makeReadFn(iso);
		expect(() =>
			// @ts-expect-error - source is intentionally omitted to assert
			// the runtime contract for callers not using TS.
			ConversionSession.open(readFn, iso.length, { format: 'god' }),
		).toThrow(/source format must be resolved/);
	});
});

describe('ConversionSession(god) with minimal XBE fixture', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);

	it('detectFormat resolves this fixture as xiso (the source shape god conversion consumes)', () => {
		expect(detectFormat(readFn, iso.length)).toBe('xiso');
	});

	it('opens without throwing', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		session.free();
	});

	it('totalUnits (part count) is at least 1', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		expect(session.totalUnits()).toBeGreaterThanOrEqual(1);
		session.free();
	});

	it('is not done immediately', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		expect(session.isDone()).toBe(false);
		session.free();
	});

	it('drains to completion and produces a non-empty final header chunk', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		driveHashing(session);
		const chunks: Uint8Array[] = [];
		while (!session.isDone()) {
			const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE);
			if (chunk) chunks.push(chunk);
		}
		session.free();
		expect(chunks.length).toBeGreaterThan(0);
		const header = chunks[chunks.length - 1];
		expect(header.length).toBeGreaterThan(0);
	});

	it('currentEntryName tracks which output file the last chunk belongs to', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		const seenNames = new Set<string>();
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
			const name = session.currentEntryName();
			expect(name).not.toBeNull();
			seenNames.add(name as string);
		}
		session.free();
		const manifestNames = new Set(manifest.map((e) => e.name));
		for (const name of seenNames) {
			expect(manifestNames.has(name)).toBe(true);
		}
	});

	it('nextChunk returns null once isDone is true', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		driveHashing(session);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		expect(session.nextChunk(1)).toBeNull();
		session.free();
	});
});

describe('ConversionSession(god) outputManifest', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);

	it('is available immediately after open, before any nextChunk call', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest.length).toBeGreaterThan(0);
	});

	it('has one entry per part plus one header entry', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		const totalUnits = session.totalUnits();
		session.free();
		expect(manifest.length).toBe(totalUnits + 1);
	});

	it('part entries are named <prefix>.data/DataNNNN', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		const partEntries = manifest.slice(0, -1);
		for (const entry of partEntries) {
			expect(entry.name).toMatch(
				/^[0-9A-F]{8}\/[0-9A-F]{8}\/[0-9A-F]{8}\.data\/Data\d{4}$/,
			);
			expect(entry.size).toBeGreaterThan(0);
		}
	});

	it('the last entry is the header, named exactly the output path prefix', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		const header = manifest[manifest.length - 1];
		expect(header.name).toMatch(/^[0-9A-F]{8}\/[0-9A-F]{8}\/[0-9A-F]{8}$/);
		expect(header.size).toBeGreaterThan(0);
	});

	it('manifest names match the names actually used during streaming', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		driveHashing(session);
		while (!session.isDone()) session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		const manifestAfter = session.outputManifest();
		session.free();
		expect(manifestAfter).toEqual(manifest);
	});
});
