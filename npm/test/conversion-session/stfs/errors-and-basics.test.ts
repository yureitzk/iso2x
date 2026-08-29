import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import {
	makeReadFn,
	nullReadFn,
	throwingReadFn,
} from '../../utils/read-fns.js';
import {
	driveHashing,
	driveAndDrain,
	expectStfsOutputDeterministicIgnoringTimestamps,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import { ConversionSession } from '../../../dist/index.js';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

describe('ConversionSession(stfs) error paths', () => {
	it('throws for a zeroed (invalid) xiso source image', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				10 * 1024 * 1024,
				{ format: 'stfs' },
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('propagates errors thrown inside readFn', () => {
		expect(() =>
			ConversionSession.open(
				throwingReadFn,
				10 * 1024 * 1024,
				{ format: 'stfs' },
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('throws for a zero file size', () => {
		expect(() =>
			ConversionSession.open(nullReadFn, 0, { format: 'stfs' }, XISO_SOURCE),
		).toThrow();
	});

	it('throws when source is omitted, instead of silently assuming xiso', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const readFn = makeReadFn(iso);
		expect(() =>
			// @ts-expect-error - source is intentionally omitted to assert
			// the runtime contract for callers not using TS.
			ConversionSession.open(readFn, iso.length, { format: 'stfs' }),
		).toThrow(/source format must be resolved/);
	});

	// Resolving a title ID normally means parsing the source's launch
	// executable. A source with neither (e.g. a DLC-only 'extracted'
	// source) has nothing for that to parse - this is only a hard error
	// when the resolved content type actually requires a launch executable
	// (see `ContentType::requires_launch_executable`). With no titleId or
	// contentType override, content type is unresolved, so this falls
	// through to titleId = 0 instead of erroring; see the two tests below
	// for the still-throws / still-defaults split.
	it('does not throw when the source has no launch executable, no titleId override, and no bootable contentType override', () => {
		const data = new Uint8Array(0x100);
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'stfs' },
				{
					source: { format: 'extracted' },
					parts: [
						{ name: 'readme.txt', size: data.length, readFn: makeReadFn(data) },
					],
				},
			),
		).not.toThrow();
	});

	// An explicit bootable contentType override makes
	// requires_launch_executable() true again, so a missing executable and
	// no titleId override is still an error.
	it('throws when contentType is explicitly overridden to a bootable type and no launch executable or titleId override is given', () => {
		const data = new Uint8Array(0x100);
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'stfs', contentType: 'gamesOnDemand' },
				{
					source: { format: 'extracted' },
					parts: [
						{ name: 'readme.txt', size: data.length, readFn: makeReadFn(data) },
					],
				},
			),
		).toThrow(/title/i);
	});

	it('does not throw when contentType is explicitly overridden to a non-bootable type and no launch executable or titleId override is given', () => {
		const data = new Uint8Array(0x100);
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'stfs', contentType: 'gamerPicture' },
				{
					source: { format: 'extracted' },
					parts: [
						{ name: 'readme.txt', size: data.length, readFn: makeReadFn(data) },
					],
				},
			),
		).not.toThrow();
	});

	it('does not throw when titleId is given explicitly, even with no launch executable present', () => {
		const data = new Uint8Array(0x100);
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'stfs', titleId: 0x5a5a0001 },
				{
					source: { format: 'extracted' },
					parts: [
						{ name: 'readme.txt', size: data.length, readFn: makeReadFn(data) },
					],
				},
			),
		).not.toThrow();
	});
});

describe('ConversionSession(stfs) with minimal fixture', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);

	it('opens without throwing', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs' },
			XISO_SOURCE,
		);
		session.free();
	});

	it('reports a positive integer totalUnits', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs' },
			XISO_SOURCE,
		);
		driveHashing(session);
		expect(session.totalUnits()).toBeGreaterThan(0);
		expect(Number.isInteger(session.totalUnits())).toBe(true);
		session.free();
	});

	it('is not done immediately', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs' },
			XISO_SOURCE,
		);
		driveHashing(session);
		expect(session.isDone()).toBe(false);
		session.free();
	});

	it('nextChunk returns null once all output is consumed', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs' },
			XISO_SOURCE,
		);
		driveHashing(session);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		expect(session.nextChunk(1)).toBeNull();
		session.free();
	});

	it('chunk size does not affect final output (chunking is transport-only), aside from the embedded creation/access timestamp', () => {
		const out1 = driveAndDrain(
			ConversionSession.open(readFn, iso.length, { format: 'stfs' }, XISO_SOURCE),
			1,
		);
		const out32 = driveAndDrain(
			ConversionSession.open(readFn, iso.length, { format: 'stfs' }, XISO_SOURCE),
			32 * 1024,
		);
		const outAll = driveAndDrain(
			ConversionSession.open(readFn, iso.length, { format: 'stfs' }, XISO_SOURCE),
			UNBOUNDED_CHUNK_SIZE,
		);
		// Each of out1/out32/outAll comes from its own ConversionSession.open()
		// call, so - same as the two-session determinism test below - any two
		// of them are allowed to disagree on the embedded
		// createdTimeStamp/accessTimeStamp windows if the calls straddle a
		// millisecond boundary. Chunk size must still fully determine
		// everything else about the output. Checking both adjacent pairs is
		// enough to cover all three (out1/outAll can't differ outside the
		// timestamp windows if neither adjacent pair does).
		expectStfsOutputDeterministicIgnoringTimestamps(out1, out32);
		expectStfsOutputDeterministicIgnoringTimestamps(out32, outAll);
	});

	it('is deterministic across separate sessions, aside from the embedded creation/access timestamp', () => {
		const a = driveAndDrain(
			ConversionSession.open(readFn, iso.length, { format: 'stfs' }, XISO_SOURCE),
			4096,
		);
		const b = driveAndDrain(
			ConversionSession.open(readFn, iso.length, { format: 'stfs' }, XISO_SOURCE),
			4096,
		);
		// Two independent open() calls over the same input are allowed to
		// disagree on the one genuinely time-dependent value STFS output
		// carries (createdTimeStamp/accessTimeStamp - see
		// stfsMinimalFixtureTimestampOffsets's doc comment in
		// fixtures/stfs.ts) - everything else must still match exactly.
		expectStfsOutputDeterministicIgnoringTimestamps(a, b);
	});
});
