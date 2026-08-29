import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { ConversionSession } from '../../../dist/index.js';
import { makeReadFn } from '../../utils/read-fns.js';
import {
	driveHashing,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

describe('ConversionSession(god) mode: "none"/"partial"/"full" (ScrubMode)', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);

	// 'full' is intentionally not re-asserted below for the basic open/drain/
	// manifest contract - it's already covered by the default (mode-less)
	// cases in errors-and-basics.test.ts and output-format.test.ts, and the
	// "omitting mode defaults to full" test right below confirms the two are
	// equivalent.
	it('opens successfully with mode: "none"', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'none' },
			XISO_SOURCE,
		);
		expect(session.totalUnits()).toBeGreaterThanOrEqual(1);
		session.free();
	});
	it('opens successfully with mode: "partial"', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'partial' },
			XISO_SOURCE,
		);
		expect(session.totalUnits()).toBeGreaterThanOrEqual(1);
		session.free();
	});

	it('omitting mode defaults to the same totalUnits as explicit mode: "full"', () => {
		const implicit = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		const implicitUnits = implicit.totalUnits();
		implicit.free();
		const explicit = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'full' },
			XISO_SOURCE,
		);
		const explicitUnits = explicit.totalUnits();
		explicit.free();
		expect(implicitUnits).toBe(explicitUnits);
	});
	it('mode: "none" never produces a smaller part count than mode: "full"', () => {
		const none = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'none' },
			XISO_SOURCE,
		);
		const noneUnits = none.totalUnits();
		none.free();
		const full = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'full' },
			XISO_SOURCE,
		);
		const fullUnits = full.totalUnits();
		full.free();
		expect(noneUnits).toBeGreaterThanOrEqual(fullUnits);
	});
	it('mode: "partial" never produces a larger part count than mode: "none"', () => {
		const partial = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'partial' },
			XISO_SOURCE,
		);
		const partialUnits = partial.totalUnits();
		partial.free();
		const none = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'none' },
			XISO_SOURCE,
		);
		const noneUnits = none.totalUnits();
		none.free();
		expect(partialUnits).toBeLessThanOrEqual(noneUnits);
	});

	it('mode: "none" drains fully and produces a non-empty header', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'none' },
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
		expect(chunks[chunks.length - 1].length).toBeGreaterThan(0);
	});
	it('mode: "partial" drains fully and produces a non-empty header', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'partial' },
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
		expect(chunks[chunks.length - 1].length).toBeGreaterThan(0);
	});

	// gameTitle x mode isn't covered anywhere else, so 'full' stays in this
	// group unlike the groups above.
	it('mode: "none" combines with gameTitle without throwing', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'none', gameTitle: 'Test Game' },
			XISO_SOURCE,
		);
		driveHashing(session);
		while (!session.isDone()) session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		session.free();
	});
	it('mode: "partial" combines with gameTitle without throwing', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'partial', gameTitle: 'Test Game' },
			XISO_SOURCE,
		);
		driveHashing(session);
		while (!session.isDone()) session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		session.free();
	});
	it('mode: "full" combines with gameTitle without throwing', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'full', gameTitle: 'Test Game' },
			XISO_SOURCE,
		);
		driveHashing(session);
		while (!session.isDone()) session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		session.free();
	});

	// 'full' is dropped here too - already covered by
	// errors-and-basics.test.ts's "outputManifest" describe block and its
	// "manifest names match the names actually used during streaming" test.
	it('outputManifest is available immediately after open with mode: "none"', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'none' },
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest.length).toBeGreaterThan(0);
	});
	it('outputManifest is available immediately after open with mode: "partial"', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'partial' },
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest.length).toBeGreaterThan(0);
	});
	it('manifest names match the names actually used during streaming with mode: "none"', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'none' },
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		driveHashing(session);
		while (!session.isDone()) session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		const manifestAfter = session.outputManifest();
		session.free();
		expect(manifestAfter).toEqual(manifest);
	});
	it('manifest names match the names actually used during streaming with mode: "partial"', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', mode: 'partial' },
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
