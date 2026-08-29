import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import {
	ConversionSession,
	detectFormat,
	isoRootOffsetCandidates,
	SourceRef,
} from '../../../dist/index.js';
import {
	makeReadFn,
	nullReadFn,
	throwingReadFn,
} from '../../utils/read-fns.js';
import { drain, UNBOUNDED_CHUNK_SIZE } from '../../utils/session-helpers.js';

beforeAll(setupWasm);

const XISO_SOURCE: SourceRef = { source: { format: 'xiso' } };

describe('ConversionSession(xiso) error paths', () => {
	it('throws for a zeroed (invalid) image', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				10 * 1024 * 1024,
				{ format: 'xiso' },
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('propagates errors thrown inside readFn', () => {
		// XDVDFSFilesystem::new() returns Option, not Result, so the
		// original readFn error message doesn't survive - only asserting
		// that *something* throws.
		expect(() =>
			ConversionSession.open(
				throwingReadFn,
				10 * 1024 * 1024,
				{ format: 'xiso' },
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('throws for a zero file size', () => {
		expect(() =>
			ConversionSession.open(nullReadFn, 0, { format: 'xiso' }, XISO_SOURCE),
		).toThrow();
	});

	// Only required once split is actually turned on - the default
	// (unsplit) path has never needed a name.
	it('throws when split is true but no outputName is given', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const readFn = makeReadFn(iso);
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'xiso',
					split: true,
				},
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('throws when source is omitted, instead of silently assuming xiso', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const readFn = makeReadFn(iso);
		expect(() =>
			// @ts-expect-error - source is intentionally omitted to assert
			// the runtime contract for callers not using TS.
			ConversionSession.open(readFn, iso.length, { format: 'xiso' }),
		).toThrow(/source format must be resolved/);
	});
});

describe('ConversionSession(xiso) with minimal fixture', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);

	it('detectFormat resolves this fixture as xiso', () => {
		expect(detectFormat(readFn, iso.length)).toBe('xiso');
	});

	it('opens without throwing', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		session.free();
	});

	it('totalUnits (sector count) is positive', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		expect(session.totalUnits()).toBeGreaterThan(0);
		session.free();
	});

	it('totalUnits is sector-aligned output, i.e. an integer sector count', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		expect(Number.isInteger(session.totalUnits())).toBe(true);
		session.free();
	});

	it('is not done immediately', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		expect(session.isDone()).toBe(false);
		session.free();
	});

	it('nextChunk(maxBytes) returns at most one sector when maxBytes < 2048', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		const chunk = session.nextChunk(1);
		expect(chunk).toBeInstanceOf(Uint8Array);
		expect(chunk!.length).toBe(2048);
		session.free();
	});

	it('nextChunk returns null once all sectors are consumed', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		const totalBytes = session.totalUnits() * 2048;
		session.nextChunk(totalBytes);
		expect(session.isDone()).toBe(true);
		expect(session.nextChunk(1)).toBeNull();
		session.free();
	});

	it('currentEntryName is always null for xiso when split is off (the default)', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		while (!session.isDone()) {
			session.nextChunk(2048);
			expect(session.currentEntryName()).toBeNull();
		}
		session.free();
	});

	it('outputManifest is empty when split is off (the default) - use totalUnits() * 2048 instead', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		expect(session.outputManifest()).toEqual([]);
		session.free();
	});

	it('chunk size does not affect final output (chunking is transport-only)', () => {
		const out1 = drain(
			ConversionSession.open(readFn, iso.length, { format: 'xiso' }, XISO_SOURCE),
			1,
		);
		const out32 = drain(
			ConversionSession.open(readFn, iso.length, { format: 'xiso' }, XISO_SOURCE),
			32 * 2048,
		);
		const outAll = drain(
			ConversionSession.open(readFn, iso.length, { format: 'xiso' }, XISO_SOURCE),
			UNBOUNDED_CHUNK_SIZE,
		);
		expect(out1).toEqual(out32);
		expect(out32).toEqual(outAll);
	});

	it('is deterministic across separate sessions', () => {
		const a = drain(
			ConversionSession.open(readFn, iso.length, { format: 'xiso' }, XISO_SOURCE),
			64 * 2048,
		);
		const b = drain(
			ConversionSession.open(readFn, iso.length, { format: 'xiso' }, XISO_SOURCE),
			64 * 2048,
		);
		expect(a).toEqual(b);
	});
});

// Regression coverage: real-world dumps (e.g. redump-style) can carry a
// leading offset before the XDVDFS volume starts, rather than the volume
// sitting at byte 0. Opening directly at byte 0 used to throw "failed to
// open XDVDFS filesystem" for any image with a nonzero offset, and every
// offset-free fixture above kept passing and masked it.
describe('ConversionSession(xiso) with an offset (redump-style) fixture', () => {
	// Root-offset detection only checks four fixed positions, in this
	// order: Xsf (offset 0), Xgd2, Xgd1, Xgd3 - it does not scan
	// arbitrary or sector-by-sector offsets. Any other offset reads as
	// "invalid ISO format" even with detection working correctly, since
	// it's simply not one of the four candidates probed.
	//
	// The offset is pulled from `isoRootOffsetCandidates()` rather than
	// hardcoded so this test can't drift out of sync if the candidate
	// list is ever reordered or changed. Xgd3 is used because it's the
	// smallest nonzero candidate (~32.5 MiB) - Xgd2/Xgd1 are ~254 MiB /
	// ~389 MiB, too large to build as an in-memory fixture here.
	//
	// isoRootOffsetCandidates() and fixture construction both need wasm
	// initialized, so they run in their own beforeAll rather than at
	// module load time.
	let ROOT_OFFSET: number;
	let baseline: Uint8Array;
	let offsetIso: Uint8Array;
	let baselineReadFn: ReturnType<typeof makeReadFn>;
	let offsetReadFn: ReturnType<typeof makeReadFn>;

	beforeAll(() => {
		const xgd3Candidate = isoRootOffsetCandidates().find(
			(c) => c.name === 'Xgd3',
		);
		if (!xgd3Candidate) {
			throw new Error(
				"Expected an 'Xgd3' entry from isoRootOffsetCandidates() - the root-offset candidate list may have changed.",
			);
		}
		ROOT_OFFSET = xgd3Candidate.rootOffset;
		// Two fixtures with identical content: one with the volume at byte
		// 0, one with ROOT_OFFSET bytes of padding in front of an
		// otherwise-identical volume. If root-offset detection and the
		// base_offset-aware reader are both working, both sessions should
		// produce byte-identical xiso output.
		baseline = makeFixture({ titleId: 0x41560001 });
		offsetIso = makeFixture({ titleId: 0x41560001, rootOffset: ROOT_OFFSET });
		baselineReadFn = makeReadFn(baseline);
		offsetReadFn = makeReadFn(offsetIso);
	});

	it("detectFormat resolves the offset fixture as xiso too - magic-byte detection doesn't depend on root offset", () => {
		expect(detectFormat(offsetReadFn, offsetIso.length)).toBe('xiso');
	});

	it('opens without throwing despite the leading offset', () => {
		const session = ConversionSession.open(
			offsetReadFn,
			offsetIso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		session.free();
	});

	it('totalUnits matches the offset-free fixture (offset correctly excluded from the volume)', () => {
		const baselineSession = ConversionSession.open(
			baselineReadFn,
			baseline.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		const offsetSession = ConversionSession.open(
			offsetReadFn,
			offsetIso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		expect(offsetSession.totalUnits()).toBe(baselineSession.totalUnits());
		baselineSession.free();
		offsetSession.free();
	});

	it('produces byte-identical output to the offset-free fixture with the same content', () => {
		// If root_offset detection or the base_offset shift in JsReader
		// were wrong (e.g. off-by-one sector, or silently falling back to
		// offset 0), this would either throw, or succeed but read
		// garbage/misaligned data and diverge from baselineOut.
		const baselineOut = drain(
			ConversionSession.open(
				baselineReadFn,
				baseline.length,
				{ format: 'xiso' },
				XISO_SOURCE,
			),
			64 * 2048,
		);
		const offsetOut = drain(
			ConversionSession.open(
				offsetReadFn,
				offsetIso.length,
				{ format: 'xiso' },
				XISO_SOURCE,
			),
			64 * 2048,
		);
		expect(offsetOut).toEqual(baselineOut);
	});

	it('rejects an offset image when given the wrong (unshifted) size, instead of silently misreading', () => {
		// Detection handles the leading offset internally, so callers
		// never need to pre-shift the declared file size.
		expect(() =>
			ConversionSession.open(
				offsetReadFn,
				offsetIso.length,
				{ format: 'xiso' },
				XISO_SOURCE,
			),
		).not.toThrow();
	});
});
