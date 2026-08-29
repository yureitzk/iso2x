import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { ConversionSession } from '../../dist/index.js';
import type { SourceRef } from '../../dist/index.js';
import { makeReadFn } from '../utils/read-fns.js';
import {
	driveHashing,
	UNBOUNDED_CHUNK_SIZE,
} from '../utils/session-helpers.js';

beforeAll(async () => {
	await setupWasm();
});

const XISO_SOURCE: SourceRef = { source: { format: 'xiso' } };

describe('ConversionSession.unitsDone()', () => {
	it("is null before and after streaming for 'xiso', where totalUnits() alone is the progress signal", () => {
		const iso = makeFixture({ titleId: 0x554e4401 });
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		expect(session.unitsDone()).toBeNull();
		while (!session.isDone()) {
			session.nextChunk(2048);
		}
		expect(session.unitsDone()).toBeNull();
		session.free();
	});

	it("is null for 'god', even after driving hashNextPart() to completion and streaming", () => {
		const iso = makeFixture({ titleId: 0x554e4402 });
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		driveHashing(session);
		expect(session.unitsDone()).toBeNull();
		while (!session.isDone()) {
			session.nextChunk(2048);
		}
		expect(session.unitsDone()).toBeNull();
		session.free();
	});

	it("is null for 'extracted', which has no hashing/sizing pass at all", () => {
		const iso = makeFixture({ titleId: 0x554e4403 });
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'extracted' },
			XISO_SOURCE,
		);
		expect(session.unitsDone()).toBeNull();
		while (!session.isDone()) {
			session.nextChunk(2048);
		}
		expect(session.unitsDone()).toBeNull();
		session.free();
	});

	it("for 'zar', starts at 0, increases monotonically as chunks are drained, and equals totalUnits() once the session is done", () => {
		const iso = makeFixture({ titleId: 0x554e4404 });
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'zar', outputName: 'game' },
			XISO_SOURCE,
		);
		expect(session.unitsDone()).toBe(0);

		let last = 0;
		while (!session.isDone()) {
			session.nextChunk(2048);
			const done = session.unitsDone();
			expect(done).not.toBeNull();
			expect(done as number).toBeGreaterThanOrEqual(last);
			last = done as number;
		}

		expect(session.unitsDone()).toBe(session.totalUnits());
		session.free();
	});

	it("for 'zar', a larger nextChunk() cap still reports the same final unitsDone() - it tracks raw input bytes, not output chunk sizes", () => {
		const iso = makeFixture({ titleId: 0x554e4405 });

		const smallChunks = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'zar', outputName: 'game' },
			XISO_SOURCE,
		);
		while (!smallChunks.isDone()) smallChunks.nextChunk(64);
		const smallFinal = smallChunks.unitsDone();
		smallChunks.free();

		const bigChunks = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'zar', outputName: 'game' },
			XISO_SOURCE,
		);
		while (!bigChunks.isDone()) bigChunks.nextChunk(UNBOUNDED_CHUNK_SIZE);
		const bigFinal = bigChunks.unitsDone();
		bigChunks.free();

		expect(smallFinal).toBe(bigFinal);
	});
});
