import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { makeReadFn } from '../../utils/read-fns.js';
import { driveHashing, driveAndDrain } from '../../utils/session-helpers.js';
import { ConversionSession, cisoSectorSize } from '../../../dist/index.js';
import { XISO_SOURCE } from '../../utils/sources.js';

let SECTOR_SIZE: number;
beforeAll(async () => {
	await setupWasm();
	SECTOR_SIZE = cisoSectorSize();
});

const OUTPUT_NAME = 'test';

// Exercises the `mode` field on OpenConversionSessionOptions for ciso.
// Every test in errors-and-basics.test.ts/output-format.test.ts predates
// that fix and only ever exercises Full/Rebuild - none of that coverage
// overlaps with this file.
describe('ConversionSession(ciso) mode option', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);
	// Locks in the default we found missing: omitting `mode` must still
	// behave exactly like the pre-fix code path (ScrubMode::Full), so this
	// change can't silently alter output for any existing caller who
	// doesn't pass `mode` explicitly.
	it('defaults to full mode when omitted, byte-identical to an explicit mode: "full"', () => {
		const implicit = driveAndDrain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
			64 * SECTOR_SIZE,
		);

		const explicit = driveAndDrain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
					mode: 'full',
				},
				XISO_SOURCE,
			),
			64 * SECTOR_SIZE,
		);
		expect(implicit).toEqual(explicit);
	});
	// serde_wasm_bindgen should reject a string outside the ScrubMode enum
	// rather than silently falling back to a default - this is the same
	// class of bug as the missing field itself: a caller typo should be
	// loud, not swallowed.
	it('rejects an invalid mode value at open()', () => {
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
					// @ts-expect-error deliberately invalid, exercising the wasm boundary
					mode: 'bogus',
				},
				XISO_SOURCE,
			),
		).toThrow();
	});
	it('"none" mode opens without throwing and produces a positive integer totalUnits', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
				mode: 'none',
			},
			XISO_SOURCE,
		);
		expect(session.totalUnits()).toBeGreaterThan(0);
		expect(Number.isInteger(session.totalUnits())).toBe(true);
		session.free();
	});
	it('"none" mode is deterministic and chunk-size independent, same as "full"', () => {
		const a = driveAndDrain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
					mode: 'none',
				},
				XISO_SOURCE,
			),
			1,
		);
		const b = driveAndDrain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
					mode: 'none',
				},
				XISO_SOURCE,
			),
			64 * SECTOR_SIZE,
		);
		expect(a).toEqual(b);
	});
	// "none" is a straight sector copy of the *original* upload - no
	// repack, no trim - so unlike "full" (whose totalUnits reflects a
	// rebuilt XDVDFS image with no fixed relationship to the input size),
	// this has a directly checkable invariant against the raw file size.
	it('"none" mode\u2019s totalUnits matches the untrimmed input size directly', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
				mode: 'none',
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		const totalUnits = session.totalUnits();
		session.free();
		expect(totalUnits).toBe(Math.ceil(iso.length / SECTOR_SIZE));
	});
	// UNVERIFIED DEPENDENCY: scrub::scan() (which Partial calls) requires a
	// default.xbe/default.xex to be present in the walked directory tree to
	// detect Platform, or it throws "no launch executable found". Whether
	// makeFixture({ titleId }) includes one isn't confirmed here - if it
	// doesn't, this test will throw for a reason unrelated to mode
	// handling. Confirm against xsf.js before trusting this one;
	// if it doesn't include an executable, either extend makeFixture to add
	// one or build a second fixture helper specifically for Partial/Scrub
	// coverage.
	it('"partial" mode opens without throwing and produces a positive integer totalUnits', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
				mode: 'partial',
			},
			XISO_SOURCE,
		);
		expect(session.totalUnits()).toBeGreaterThan(0);
		expect(Number.isInteger(session.totalUnits())).toBe(true);
		session.free();
	});
	it('"partial" mode is deterministic and chunk-size independent', () => {
		const a = driveAndDrain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
					mode: 'partial',
				},
				XISO_SOURCE,
			),
			1,
		);
		const b = driveAndDrain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'ciso',
					outputName: OUTPUT_NAME,
					mode: 'partial',
				},
				XISO_SOURCE,
			),
			64 * SECTOR_SIZE,
		);
		expect(a).toEqual(b);
	});
});
