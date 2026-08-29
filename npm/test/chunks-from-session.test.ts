import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from './utils/wasm-setup.js';
import { makeFixture } from './utils/fixtures/xsf.js';
import { ConversionSession, chunksFromSession } from '../dist/index.js';
import type { SourceRef } from '../dist/index.js';
import { makeReadFn } from './utils/read-fns.js';
import {
	driveAndDrain,
	concat,
	UNBOUNDED_CHUNK_SIZE,
} from './utils/session-helpers.js';

beforeAll(async () => {
	await setupWasm();
});

const XISO_SOURCE: SourceRef = { source: { format: 'xiso' } };

describe('chunksFromSession()', () => {
	it('yields the same bytes, in the same order, as driving nextChunk()/isDone() directly', async () => {
		const iso = makeFixture({ titleId: 0x43460001 });

		const expected = driveAndDrain(
			ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'xiso' },
				XISO_SOURCE,
			),
			2048,
		);

		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		const chunks: Uint8Array[] = [];
		for await (const chunk of chunksFromSession(session, 2048, async () => {})) {
			chunks.push(chunk);
		}
		session.free();

		expect(concat(chunks)).toEqual(expected);
	});

	it('calls waitForRoom() once before every nextChunk() call, and stops calling it once the session is done', async () => {
		const iso = makeFixture({ titleId: 0x43460002 });
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);

		let waitCalls = 0;
		let chunkCount = 0;
		for await (const _chunk of chunksFromSession(session, 2048, async () => {
			waitCalls += 1;
		})) {
			chunkCount += 1;
		}
		session.free();

		// One waitForRoom() call precedes every yielded chunk, so the two
		// counts line up exactly - no extra call after the last real
		// chunk, and none skipped before it.
		expect(waitCalls).toBe(chunkCount);
		expect(chunkCount).toBeGreaterThan(0);
	});

	it('respects the maxBytes cap on every yielded chunk, the same as calling nextChunk(maxBytes) directly', async () => {
		// zar's output isn't sector-quantized like xiso's (see
		// xiso/errors-and-basics.test.ts's "returns at most one sector
		// when maxBytes < 2048"), so a small cap here actually exercises
		// chunksFromSession forwarding maxBytes through on every call,
		// not just the first.
		const iso = makeFixture({ titleId: 0x43460003 });
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'zar', outputName: 'game' },
			XISO_SOURCE,
		);

		const maxBytes = 37;
		let sawAny = false;
		for await (const chunk of chunksFromSession(
			session,
			maxBytes,
			async () => {},
		)) {
			sawAny = true;
			expect(chunk.length).toBeLessThanOrEqual(maxBytes);
		}
		session.free();
		expect(sawAny).toBe(true);
	});

	it('an already-exhausted session yields nothing and calls waitForRoom() zero times', async () => {
		const iso = makeFixture({ titleId: 0x43460004 });
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		// Drain it directly first, the same way driveAndDrain does, so
		// the session is done before chunksFromSession ever sees it.
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}

		let waitCalls = 0;
		const chunks: Uint8Array[] = [];
		for await (const chunk of chunksFromSession(session, 2048, async () => {
			waitCalls += 1;
		})) {
			chunks.push(chunk);
		}
		session.free();

		expect(chunks).toHaveLength(0);
		expect(waitCalls).toBe(0);
	});
});
