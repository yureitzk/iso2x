import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import {
	DEFAULT_XBE_DECLARED_SIZE,
	makeFixture,
	SYSTEM_UPDATE_FILE_NAME,
	SYSTEM_UPDATE_FILE_SIZE,
} from '../../utils/fixtures/xsf.js';
import {
	ConversionSession,
	SourceRef,
	zarBlockSize,
} from '../../../dist/index.js';
import { makeReadFn } from '../../utils/read-fns.js';
import { drain, UNBOUNDED_CHUNK_SIZE } from '../../utils/session-helpers.js';
import { readZarFileList } from '../../utils/decoders/zar.js';

beforeAll(setupWasm);

const XISO_SOURCE: SourceRef = { source: { format: 'xiso' } };

describe('ConversionSession(zar) output shape', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);

	it('outputManifest is empty before streaming completes (final size depends on per-block compression)', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'zar', outputName: 'game' },
			XISO_SOURCE,
		);
		expect(session.outputManifest()).toEqual([]);
		session.free();
	});

	it('outputManifest reports exactly one entry, named "<outputName>.zar", once done', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'zar', outputName: 'game' },
			XISO_SOURCE,
		);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		const manifest = session.outputManifest();
		session.free();
		expect(manifest).toHaveLength(1);
		expect(manifest[0].name).toBe('game.zar');
		expect(manifest[0].size).toBeGreaterThan(0);
	});

	it("outputManifest's reported size matches the sum of every drained chunk's length", () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'zar', outputName: 'game' },
			XISO_SOURCE,
		);
		let total = 0;
		while (!session.isDone()) {
			const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE);
			if (chunk) total += chunk.length;
		}
		const manifest = session.outputManifest();
		session.free();
		expect(manifest[0].size).toBe(total);
	});

	it('currentEntryName is the bare outputName (no ".zar" suffix), and is already available before any nextChunk call', () => {
		// Unlike extracted/ciso/cci, where currentEntryName() only becomes
		// meaningful once nextChunk() has been called at least once, zar's
		// implementation returns the (constant) base name unconditionally
		// - see ZarSession::current_entry_name in formats/zar.rs.
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'zar', outputName: 'game' },
			XISO_SOURCE,
		);
		expect(session.currentEntryName()).toBe('game');
		session.free();
	});

	it("currentEntryName never changes across the session's lifetime, and doesn't match outputManifest's (suffixed) name", () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'zar', outputName: 'game' },
			XISO_SOURCE,
		);
		const seen = new Set<string | null>();
		while (!session.isDone()) {
			session.nextChunk(2048);
			seen.add(session.currentEntryName());
		}
		const manifest = session.outputManifest();
		session.free();
		expect(seen.size).toBe(1);
		expect([...seen][0]).toBe('game');
		expect(manifest[0].name).toBe(`${[...seen][0]}.zar`);
	});

	it("zarBlockSize() is 64 KiB, matching formats/zar.rs's BLOCK_SIZE", () => {
		expect(zarBlockSize()).toBe(64 * 1024);
	});

	it('chunk size does not affect final output (chunking is transport-only)', () => {
		const out1 = drain(
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'zar', outputName: 'game' },
				XISO_SOURCE,
			),
			1,
		);
		const outAll = drain(
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'zar', outputName: 'game' },
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		expect(out1).toEqual(outAll);
	});

	it('is deterministic across separate sessions with identical input', () => {
		const a = drain(
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'zar', outputName: 'game' },
				XISO_SOURCE,
			),
			64 * 1024,
		);
		const b = drain(
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'zar', outputName: 'game' },
				XISO_SOURCE,
			),
			64 * 1024,
		);
		expect(a).toEqual(b);
	});

	it('output differs when input content differs', () => {
		const isoA = makeFixture({ titleId: 0x41560001 });
		const isoB = makeFixture({ titleId: 0xdeadbeef });
		const outA = drain(
			ConversionSession.open(
				makeReadFn(isoA),
				isoA.length,
				{ format: 'zar', outputName: 'game' },
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		const outB = drain(
			ConversionSession.open(
				makeReadFn(isoB),
				isoB.length,
				{ format: 'zar', outputName: 'game' },
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		expect(outA).not.toEqual(outB);
	});

	it('outputName only changes the reported filename, not the archived content', () => {
		const outGame = drain(
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'zar', outputName: 'game' },
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		const outOther = drain(
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'zar', outputName: 'totally-different-name' },
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		// The base_name isn't written into the archive body anywhere the
		// spec cares about (it's only ever used for the reported
		// name/currentEntryName), so two conversions of identical content
		// should still produce byte-identical archives regardless of what
		// they're named.
		expect(outGame).toEqual(outOther);
	});

	it('totalUnits sums declared sizes across multiple files (default.xbe + $SystemUpdate file)', () => {
		const withUpdate = makeFixture({
			titleId: 0x41560001,
			includeSystemUpdate: true,
		});
		const session = ConversionSession.open(
			makeReadFn(withUpdate),
			withUpdate.length,
			{ format: 'zar', outputName: 'game' },
			XISO_SOURCE,
		);
		expect(session.totalUnits()).toBe(
			DEFAULT_XBE_DECLARED_SIZE + SYSTEM_UPDATE_FILE_SIZE,
		);
		session.free();
	});

	it('round-trips every file through zar packing without silently dropping one from a subdirectory', () => {
		const withUpdate = makeFixture({
			titleId: 0x41560005,
			includeSystemUpdate: true,
		});
		const zarBytes = drain(
			ConversionSession.open(
				makeReadFn(withUpdate),
				withUpdate.length,
				{ format: 'zar', outputName: 'game' },
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		const files = readZarFileList(zarBytes);
		expect(files).toContainEqual({
			path: 'default.xbe',
			size: DEFAULT_XBE_DECLARED_SIZE,
		});
		expect(files).toContainEqual({
			path: `$SystemUpdate/${SYSTEM_UPDATE_FILE_NAME}`,
			size: SYSTEM_UPDATE_FILE_SIZE,
		});
		expect(files).toHaveLength(2);
	});
});
