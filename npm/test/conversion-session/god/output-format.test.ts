import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { ConversionSession, mhtSize } from '../../../dist/index.js';
import { makeReadFn } from '../../utils/read-fns.js';
import {
	driveHashing,
	drainNamed,
	concat,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

describe('ConversionSession(god) part files include the leading master MHT header', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);
	it('every DataNNNN entry starts with an mhtSize()-byte chunk before any subpart data', () => {
		const MHT_SIZE = mhtSize();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		const partNames = manifest.slice(0, -1).map((e) => e.name);
		const firstChunkByName = new Map<string, Uint8Array>();
		while (!session.isDone()) {
			const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE);
			if (!chunk) break;
			const name = session.currentEntryName() as string;
			if (!firstChunkByName.has(name)) firstChunkByName.set(name, chunk);
		}
		session.free();
		for (const name of partNames) {
			const first = firstChunkByName.get(name);
			expect(first, `no chunks recorded for ${name}`).toBeDefined();
			expect((first as Uint8Array).length).toBe(MHT_SIZE);
		}
	});
	it('each part file size matches its manifest size once header + subparts are concatenated', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		const byName = drainNamed(session);
		session.free();
		for (const entry of manifest.slice(0, -1)) {
			const chunks = byName.get(entry.name);
			expect(chunks, `no chunks recorded for ${entry.name}`).toBeDefined();
			const full = concat(chunks as Uint8Array[]);
			expect(full.length).toBe(entry.size);
		}
	});
	it('the master MHT header bytes are not all-zero (i.e. were actually computed)', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		driveHashing(session);
		const byName = drainNamed(session);
		session.free();
		for (const [name, chunks] of byName) {
			if (!name.endsWith('.data/Data0000')) continue;
			const header = chunks[0];
			const allZero = header.every((b) => b === 0);
			expect(allZero, `master MHT for ${name} is all zero`).toBe(false);
		}
	});
	it('first chunk per part is exactly mhtSize() bytes, distinguishing it from any subpart chunk', () => {
		const MHT_SIZE = mhtSize();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		driveHashing(session);
		const byName = drainNamed(session);
		session.free();
		for (const [name, chunks] of byName) {
			if (!name.includes('.data/Data')) continue;
			expect(chunks.length).toBeGreaterThan(1);
			expect(chunks[0].length).toBe(MHT_SIZE);
			for (let i = 1; i < chunks.length; i++) {
				expect(chunks[i].length).not.toBe(MHT_SIZE);
			}
		}
	});
});
