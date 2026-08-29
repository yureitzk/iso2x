import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import {
	openSource,
	inspectSource,
	generateAttachXbe,
	ConversionSession,
} from '../../dist/index.js';
import type { SourceRef } from '../../dist/index.js';
import type { ConversionSession as RawConversionSession } from '../../dist/wasm/iso2x.js';
import { makeReadFn, scan, only } from '../utils/read-fns.js';
import {
	driveAndDrain,
	convertXisoFixtureToGodParts,
	concat,
	UNBOUNDED_CHUNK_SIZE,
} from '../utils/session-helpers.js';
import { checkIsoCompleteness } from '../../dist/detect-advanced.js';

beforeAll(async () => {
	await setupWasm();
});

const XISO_SOURCE: SourceRef = { source: { format: 'xiso' } };

/**
 * Drains a session obtained from `ConversionSession.open(...)` (the
 * wrapped session-helpers.ts type).
 */
function drainWrapped(session: ConversionSession): Uint8Array {
	return driveAndDrain(session, UNBOUNDED_CHUNK_SIZE);
}

/**
 * `OpenedSource.openConversionSession()` returns the raw wasm-bindgen
 * `ConversionSession` class, not the wrapped one `session-helpers.ts`'s
 * `driveAndDrain`/`drain` are typed against, so this duplicates the
 * same drive/drain loop against the raw type directly.
 *
 * Most callers should reach for `ConversionSession.wrap()` instead (see
 * the "ConversionSession.wrap()" describe block below). This raw-typed
 * duplicate stays only to exercise the wasm-bindgen class's own shape
 * directly.
 */
function drainRaw(session: RawConversionSession): Uint8Array {
	while (!session.hashNextPart()) {
		/* keep driving the sizing pass */
	}
	const chunks: Uint8Array[] = [];
	while (!session.isDone()) {
		const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		if (chunk) chunks.push(chunk);
	}
	session.free();
	return concat(chunks);
}

describe('openSource() -> OpenedSource chaining', () => {
	it('inspect() matches standalone inspectSource() for the same bytes', () => {
		const iso = makeFixture({ titleId: 0x4f530001 });
		const expected = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE);

		const handle = openSource(makeReadFn(iso), iso.length, { format: 'xiso' });
		try {
			const info = handle.inspect(false);
			expect(info.titleId).toBe(expected.titleId);
			expect(info.contentType).toBe(expected.contentType);
		} finally {
			handle.free();
		}
	});

	it('generateAttachXbe() on the handle matches the standalone function for the same bytes', () => {
		const iso = makeFixture({ titleId: 0x4f530002 });
		const expected = generateAttachXbe(makeReadFn(iso), iso.length, XISO_SOURCE);

		const handle = openSource(makeReadFn(iso), iso.length, { format: 'xiso' });
		try {
			expect(handle.generateAttachXbe()).toEqual(expected);
		} finally {
			handle.free();
		}
	});

	it('inspect() then generateAttachXbe() both still work on the same handle - inspect() only borrows', () => {
		const iso = makeFixture({ titleId: 0x4f530003 });
		const handle = openSource(makeReadFn(iso), iso.length, { format: 'xiso' });
		try {
			const info = handle.inspect(false);
			expect(info.titleId).toBe('4F530003');
			// If inspect() had consumed the handle instead of borrowing it,
			// this second call would throw.
			expect(() => handle.generateAttachXbe()).not.toThrow();
		} finally {
			handle.free();
		}
	});

	it('openConversionSession() on the handle produces the same bytes as a fresh standalone ConversionSession', () => {
		const iso = makeFixture({ titleId: 0x4f530004 });

		const expectedSession = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		const expected = drainWrapped(expectedSession);

		const handle = openSource(makeReadFn(iso), iso.length, { format: 'xiso' });
		const session = handle.openConversionSession({ format: 'xiso' });
		const actual = drainRaw(session);

		expect(actual).toEqual(expected);
	});

	it('inspect() before openConversionSession() still yields a session producing correct output - the cached directory-table walk carries forward', () => {
		const iso = makeFixture({ titleId: 0x4f530005 });
		const handle = openSource(makeReadFn(iso), iso.length, { format: 'xiso' });

		const info = handle.inspect(false);
		expect(info.titleId).toBe('4F530005');

		const session = handle.openConversionSession({ format: 'xiso' });
		const bytes = drainRaw(session);

		expect(bytes.length).toBeGreaterThan(0);
	});

	it('openConversionSession() consumes the handle - a further call throws instead of silently reusing a freed source', () => {
		const iso = makeFixture({ titleId: 0x4f530006 });
		const handle = openSource(makeReadFn(iso), iso.length, { format: 'xiso' });

		const session = handle.openConversionSession({ format: 'xiso' });
		session.free();

		expect(() => handle.inspect(false)).toThrow();
	});
});

describe('ConversionSession.wrap()', () => {
	it('wraps the raw session from OpenedSource.openConversionSession() into output matching a fresh standalone ConversionSession.open()', () => {
		const iso = makeFixture({ titleId: 0x4f530007 });

		const expectedSession = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		const expected = drainWrapped(expectedSession);

		const handle = openSource(makeReadFn(iso), iso.length, { format: 'xiso' });
		const wrapped = ConversionSession.wrap(
			handle.openConversionSession({ format: 'xiso' }),
		);
		const actual = driveAndDrain(wrapped, UNBOUNDED_CHUNK_SIZE);

		expect(actual).toEqual(expected);
	});

	it('reports currentEntryName() as null, not undefined, for a single-stream format (xiso) - the raw session reports undefined there', () => {
		const iso = makeFixture({ titleId: 0x4f530008 });
		const handle = openSource(makeReadFn(iso), iso.length, { format: 'xiso' });
		const wrapped = ConversionSession.wrap(
			handle.openConversionSession({ format: 'xiso' }),
		);

		const chunk = wrapped.nextChunk(UNBOUNDED_CHUNK_SIZE);
		expect(chunk).not.toBeNull();
		// The raw session reports `undefined` here, so a caller doing a
		// strict `=== null` check against it would never see a match.
		expect(wrapped.currentEntryName()).toBeNull();

		wrapped.free();
	});

	it('reports unitsDone() as null, not undefined, for a format that does not track it (xiso) - the raw session reports undefined there', () => {
		const iso = makeFixture({ titleId: 0x4f530009 });
		const handle = openSource(makeReadFn(iso), iso.length, { format: 'xiso' });
		const wrapped = ConversionSession.wrap(
			handle.openConversionSession({ format: 'xiso' }),
		);

		wrapped.nextChunk(UNBOUNDED_CHUNK_SIZE);
		// Same shape of bug as currentEntryName() above: a caller doing
		// `unitsDone() !== null` to fall back to summing chunk lengths
		// would never take that branch against the raw session, since
		// `undefined !== null` is true.
		expect(wrapped.unitsDone()).toBeNull();

		wrapped.free();
	});

	it('reports nextChunk() as null, not undefined, once the wrapped session is exhausted', () => {
		const iso = makeFixture({ titleId: 0x4f53000a });
		const handle = openSource(makeReadFn(iso), iso.length, { format: 'xiso' });
		const wrapped = ConversionSession.wrap(
			handle.openConversionSession({ format: 'xiso' }),
		);

		while (!wrapped.isDone()) wrapped.nextChunk(UNBOUNDED_CHUNK_SIZE);
		expect(wrapped.nextChunk(UNBOUNDED_CHUNK_SIZE)).toBeNull();

		wrapped.free();
	});

	it('free() on the wrapped session frees the underlying raw session - a second free() throws the same way it would on the raw session directly', () => {
		const iso = makeFixture({ titleId: 0x4f53000b });
		const handle = openSource(makeReadFn(iso), iso.length, { format: 'xiso' });
		const wrapped = ConversionSession.wrap(
			handle.openConversionSession({ format: 'xiso' }),
		);

		wrapped.free();
		expect(() => wrapped.free()).toThrow('null pointer passed to rust');
	});
});

describe('resolveBatch() handle - standalone raw XISO', () => {
	it('carries a live, working handle for a lone complete image', async () => {
		const iso = makeFixture({ titleId: 0x52585801 });
		const results = await scan({ 'solo.iso': iso });

		const standalone = only(results, 'standalone');
		expect(standalone.titleId).toBe('52585801');
		expect(standalone.handle).toBeDefined();

		const info = standalone.handle!.inspect(false);
		expect(info.titleId).toBe('52585801');

		const expectedAttach = generateAttachXbe(
			makeReadFn(iso),
			iso.length,
			XISO_SOURCE,
		);
		expect(standalone.handle!.generateAttachXbe()).toEqual(expectedAttach);
	});

	it("a raw-XISO handle's conversion session output matches a fresh standalone conversion of the same bytes", async () => {
		const iso = makeFixture({ titleId: 0x52585802 });
		const results = await scan({ 'solo.iso': iso });
		const standalone = only(results, 'standalone');

		const expectedSession = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		const expected = drainWrapped(expectedSession);

		const session = standalone.handle!.openConversionSession({
			format: 'xiso',
		});
		const actual = drainRaw(session);

		expect(actual).toEqual(expected);
	});
});

describe('resolveBatch() handle - standalone GOD candidate', () => {
	it('two distinct-titleId GOD folders each resolve as their own Standalone with a working handle', async () => {
		const isoA = makeFixture({ titleId: 0x474f4401, platform: 'x360' });
		const isoB = makeFixture({ titleId: 0x474f4402, platform: 'x360' });
		const { dataParts: dataA } = convertXisoFixtureToGodParts(isoA);
		const { dataParts: dataB } = convertXisoFixtureToGodParts(isoB);
		expect(dataA).toHaveLength(1);
		expect(dataB).toHaveLength(1);

		const files: Record<string, Uint8Array> = {
			'TitleA.data/Data0000': dataA[0]!.readFn(0, dataA[0]!.size),
			'TitleB.data/Data0000': dataB[0]!.readFn(0, dataB[0]!.size),
		};
		const results = await scan(files);

		expect(results.filter((r) => r.kind === 'godFolder')).toHaveLength(0);
		const standalones = results.filter((r) => r.kind === 'standalone');
		expect(standalones).toHaveLength(2);

		const titleIds = standalones
			.map((r) => (r.kind === 'standalone' ? r.titleId : undefined))
			.sort();
		expect(titleIds).toEqual(['474F4401', '474F4402']);

		for (const s of standalones) {
			if (s.kind !== 'standalone') continue;
			expect(s.handle).toBeDefined();
			const info = s.handle!.inspect(false);
			expect(info.titleId).toBe(s.titleId);
			expect(info.contentType).toBe('gamesOnDemand');
		}
	});

	it('a single unpaired GOD folder still resolves as GodFolder with no handle - deliberately unverified', async () => {
		const iso = makeFixture({ titleId: 0x474f4403, platform: 'x360' });
		const { dataParts } = convertXisoFixtureToGodParts(iso);
		const results = await scan({
			'Solo.data/Data0000': dataParts[0]!.readFn(0, dataParts[0]!.size),
		});

		const godFolder = only(results, 'godFolder');
		expect(godFolder.names).toEqual(['Solo.data/Data0000']);
		// `GodFolder` has no `handle` field at all in the type - nothing
		// to assert beyond the shape check above.
	});
});

describe('resolveBatch() handle - resolved raw split', () => {
	function splitFixture(titleId: number): {
		whole: Uint8Array;
		header: Uint8Array;
		continuation: Uint8Array;
	} {
		const whole = makeFixture({ titleId });
		const info = checkIsoCompleteness(makeReadFn(whole), whole.length);
		expect(info).toBeDefined();
		expect(info!.isComplete).toBe(true);
		const cut = info!.rootOffset + info!.maxUsedPrefixSize - 0x400;
		return {
			whole,
			header: whole.slice(0, cut),
			continuation: whole.slice(cut),
		};
	}

	it('carries a live handle for the winning ordering, equivalent to the unfragmented source', async () => {
		const { whole, header, continuation } = splitFixture(0x52535031);
		const results = await scan({
			'split.1.iso': header,
			'split.2.iso': continuation,
		});

		const rawSplit = only(results, 'rawSplit');
		expect(rawSplit.parts.slice().sort()).toEqual(
			['split.1.iso', 'split.2.iso'].sort(),
		);
		expect(rawSplit.verify.ok).toBe(true);
		expect(rawSplit.handle).toBeDefined();

		const info = rawSplit.handle!.inspect(false);
		expect(info.titleId).toBe('52535031');

		const expectedAttach = generateAttachXbe(
			makeReadFn(whole),
			whole.length,
			XISO_SOURCE,
		);
		expect(rawSplit.handle!.generateAttachXbe()).toEqual(expectedAttach);
	});

	it("a raw-split handle's conversion session output matches converting the reassembled whole image directly", async () => {
		const { whole, header, continuation } = splitFixture(0x52535032);
		const results = await scan({
			'split.1.iso': header,
			'split.2.iso': continuation,
		});
		const rawSplit = only(results, 'rawSplit');

		const expectedSession = ConversionSession.open(
			makeReadFn(whole),
			whole.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		const expected = drainWrapped(expectedSession);

		const session = rawSplit.handle!.openConversionSession({ format: 'xiso' });
		const actual = drainRaw(session);

		expect(actual).toEqual(expected);
	});
});
