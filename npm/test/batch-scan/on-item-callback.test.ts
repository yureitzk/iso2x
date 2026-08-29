import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { scanBatch, resolveBatchEntry } from '../../dist/index.js';
import type { BatchResolution } from '../../dist/index.js';
import { nameMap } from '../utils/read-fns.js';
import { patchXexExecutionInfo } from '../utils/session-helpers.js';

beforeAll(async () => {
	await setupWasm();
});

// `resolveBatch`'s (JS: `scanBatch`'s) `onItem` callback is documented -
// "called synchronously as soon as each result not needing whole-batch
// correlation is known" - but nothing else in the suite ever passes one,
// so the sharpest edge of its contract (see the last describe block
// below) was completely unverified: once a result has been handed to
// `onItem`, it is NOT also present in scanBatch's resolved array. A
// caller who assumes the resolved array is the complete picture when
// they've supplied `onItem` will silently lose every item reported
// through the callback.
describe('scanBatch() onItem callback', () => {
	it('fires once for a lone standalone result - and that result is then absent from the resolved array', async () => {
		const iso = makeFixture({ titleId: 0x4f490001 });
		const seen: BatchResolution[] = [];

		const results = await scanBatch(
			['solo.iso'],
			nameMap({ 'solo.iso': iso }),
			(r) => {
				seen.push(r);
			},
		);

		expect(seen).toHaveLength(1);
		expect(seen[0].kind).toBe('standalone');
		// Not a partial/duplicate - the resolved array is empty because
		// its one result was already delivered via onItem.
		expect(results).toHaveLength(0);
	});

	it('fires independently for each standalone in a batch of several, and none of them end up in the resolved array either', async () => {
		const isoA = makeFixture({ titleId: 0x4f490002 });
		const isoB = makeFixture({ titleId: 0x4f490003 });
		const seenKinds: string[] = [];

		const results = await scanBatch(
			['a.iso', 'b.iso'],
			nameMap({ 'a.iso': isoA, 'b.iso': isoB }),
			(r) => {
				seenKinds.push(r.kind);
			},
		);

		expect(seenKinds.sort()).toEqual(['standalone', 'standalone']);
		expect(results).toHaveLength(0);
	});

	it('is NOT called for a MultiDiscSet - grouping needs the whole batch, so that result only ever appears in the resolved array', async () => {
		const titleId = 0x4f490004;
		const disc1 = patchXexExecutionInfo(
			makeFixture({ titleId, platform: 'x360' }),
			{ discNumber: 1, discCount: 2, mediaId: 0x1 },
		);
		const disc2 = patchXexExecutionInfo(
			makeFixture({ titleId, platform: 'x360' }),
			{ discNumber: 2, discCount: 2, mediaId: 0x2 },
		);
		const seen: BatchResolution[] = [];

		const results = await scanBatch(
			['Game (Disc 1).iso', 'Game (Disc 2).iso'],
			nameMap({ 'Game (Disc 1).iso': disc1, 'Game (Disc 2).iso': disc2 }),
			(r) => {
				seen.push(r);
			},
		);

		expect(seen).toHaveLength(0);
		expect(results).toHaveLength(1);
		expect(results[0].kind).toBe('multiDiscSet');
	});

	it('in a mixed batch, onItem plus the resolved array together account for every result - neither alone does', async () => {
		const solo = makeFixture({ titleId: 0x4f490005 });
		const titleId = 0x4f490006;
		const disc1 = patchXexExecutionInfo(
			makeFixture({ titleId, platform: 'x360' }),
			{ discNumber: 1, discCount: 2, mediaId: 0x1 },
		);
		const disc2 = patchXexExecutionInfo(
			makeFixture({ titleId, platform: 'x360' }),
			{ discNumber: 2, discCount: 2, mediaId: 0x2 },
		);
		const files = {
			'solo.iso': solo,
			'Set/Disc1.iso': disc1,
			'Set/Disc2.iso': disc2,
		};
		const seen: BatchResolution[] = [];

		const results = await scanBatch(Object.keys(files), nameMap(files), (r) => {
			seen.push(r);
		});

		// The standalone came through onItem only; the multiDiscSet came
		// through the resolved array only. Reading just one side would
		// silently miss the other.
		expect(seen.map((r) => r.kind)).toEqual(['standalone']);
		expect(results.map((r) => r.kind)).toEqual(['multiDiscSet']);
	});

	it('omitting onItem entirely still returns every result in the resolved array, standalone and MultiDiscSet alike', async () => {
		const solo = makeFixture({ titleId: 0x4f490007 });
		const titleId = 0x4f490008;
		const disc1 = patchXexExecutionInfo(
			makeFixture({ titleId, platform: 'x360' }),
			{ discNumber: 1, discCount: 2, mediaId: 0x1 },
		);
		const disc2 = patchXexExecutionInfo(
			makeFixture({ titleId, platform: 'x360' }),
			{ discNumber: 2, discCount: 2, mediaId: 0x2 },
		);
		const files = {
			'solo.iso': solo,
			'Set/Disc1.iso': disc1,
			'Set/Disc2.iso': disc2,
		};

		const results = await scanBatch(Object.keys(files), nameMap(files));

		expect(results.map((r) => r.kind).sort()).toEqual([
			'multiDiscSet',
			'standalone',
		]);
	});
});

describe('scanBatch()/resolveBatchEntry() with an empty entries array', () => {
	it('scanBatch throws rather than resolving to an empty array', async () => {
		await expect(scanBatch([], nameMap({}))).rejects.toThrow();
	});

	it('resolveBatchEntry throws rather than resolving to some default/empty result', async () => {
		await expect(resolveBatchEntry([], nameMap({}))).rejects.toThrow();
	});
});
