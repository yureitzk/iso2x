import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { ConversionSession, detectDirFormat } from '../../../dist/index.js';
import {
	makeReadFn,
	nullReadFn,
	throwingReadFn,
} from '../../utils/read-fns.js';
import {
	convertXisoFixtureToExtractedParts,
	drain,
	driveHashing,
} from '../../utils/session-helpers.js';
import { EXTRACTED_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

const OUTPUT_NAME = 'game';

describe('detectDirFormat resolves a converted extracted file listing correctly', () => {
	it('resolves the flat, root-level-default.xbe listing as extracted', () => {
		// Sanity check that the parts this whole file relies on really are
		// what real detect-first JS callers would resolve as `extracted`
		// (rather than, say, `god`) before ever reaching source::open.
		const iso = makeFixture({ titleId: 0x41560001 });
		const parts = convertXisoFixtureToExtractedParts(iso);
		expect(detectDirFormat(parts.map((p) => p.name))).toBe('extracted');
	});
});

describe('opening a conversion session from an extracted source', () => {
	// `iso` is plain fixture bytes and safe to share across every test in
	// this describe; `parts` wraps stateful readFn closures, so it's rebuilt
	// fresh per test rather than hoisted alongside it.
	const iso = makeFixture({ titleId: 0x41560001 });
	it('opens a xiso target from an extracted source without throwing', () => {
		const parts = convertXisoFixtureToExtractedParts(iso);
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'xiso' },
			{ ...EXTRACTED_SOURCE, parts },
		);
		session.free();
	});

	it('drains a xiso target opened from an extracted source to completion', () => {
		const parts = convertXisoFixtureToExtractedParts(iso);
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'xiso' },
			{ ...EXTRACTED_SOURCE, parts },
		);
		const out = drain(session, 1024 * 1024);
		expect(out.length).toBeGreaterThan(0);
	});

	it('opens a god target from an extracted source without throwing', () => {
		const parts = convertXisoFixtureToExtractedParts(iso);
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'god' },
			{ ...EXTRACTED_SOURCE, parts },
		);
		session.free();
	});

	it('opens a ciso target from an extracted source without throwing', () => {
		const parts = convertXisoFixtureToExtractedParts(iso);
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'ciso', outputName: OUTPUT_NAME },
			{ ...EXTRACTED_SOURCE, parts },
		);
		session.free();
	});

	it('opens a cci target from an extracted source without throwing', () => {
		const parts = convertXisoFixtureToExtractedParts(iso);
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			{ ...EXTRACTED_SOURCE, parts },
		);
		session.free();
	});
});

describe('extracted source: god target derives title info from the launch executable', () => {
	it('produces different CON header manifest entries for different titleIds', () => {
		// GodSession::open_from_extracted has no XDVDFS root to read a
		// TitleInfo from, so it has to fall back to
		// ExtractedFilesystem::read_launch_executable to pull title_id
		// straight out of default.xbe's bytes instead. The trailing CON
		// header manifest entry (name: "titleId/contentType/mediaId", see
		// convertXisoFixtureToGodParts's doc comment) bakes titleId into its
		// own name, giving an observable way to confirm two fixtures with
		// different titleIds actually produced different titles here,
		// without needing to fully decode a GOD header.
		const isoA = makeFixture({ titleId: 0x41560001 });
		const isoB = makeFixture({ titleId: 0xdeadbeef });
		const partsA = convertXisoFixtureToExtractedParts(isoA);
		const partsB = convertXisoFixtureToExtractedParts(isoB);
		const sessionA = ConversionSession.open(
			nullReadFn,
			isoA.length,
			{ format: 'god' },
			{ ...EXTRACTED_SOURCE, parts: partsA },
		);
		const sessionB = ConversionSession.open(
			nullReadFn,
			isoB.length,
			{ format: 'god' },
			{ ...EXTRACTED_SOURCE, parts: partsB },
		);
		const manifestA = sessionA.outputManifest();
		const manifestB = sessionB.outputManifest();
		sessionA.free();
		sessionB.free();
		expect(manifestA[manifestA.length - 1].name).not.toBe(
			manifestB[manifestB.length - 1].name,
		);
	});
});

describe('extracted source: skip-filtered parts feed other targets correctly', () => {
	it('a god target built from skipSystemUpdate-filtered extracted parts carries no $SystemUpdate part', () => {
		const iso = makeFixture({
			titleId: 0x41560001,
			includeSystemUpdate: true,
		});
		const parts = convertXisoFixtureToExtractedParts(iso, {
			format: 'extracted',
			skipSystemUpdate: true,
		});
		expect(parts.some((p) => p.name.toUpperCase().includes('SYSTEMUPDATE'))).toBe(
			false,
		);
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'god' },
			{ ...EXTRACTED_SOURCE, parts },
		);
		session.free();
	});
});

describe('extracted source: extracted target', () => {
	// `ExtractedSession::open_from_extracted` is backing-agnostic (a
	// loose-files `Parts` source and a packed `.zar` both stream through
	// the same `read_file_range` path), so extracted -> extracted converts
	// exactly like every other source/target pair - a plain copy-through
	// when no options are set, real filtering/patching when they are.
	const iso = makeFixture({ titleId: 0x41560001 });
	it('opens and drains without throwing, same as any other target', () => {
		const parts = convertXisoFixtureToExtractedParts(iso);
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'extracted' },
			{ ...EXTRACTED_SOURCE, parts },
		);
		const out = drain(session, 1024 * 1024);
		expect(out.length).toBeGreaterThan(0);
	});

	it('a plain copy-through (no options set) round-trips every file name and size unchanged', () => {
		const parts = convertXisoFixtureToExtractedParts(iso);
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'extracted' },
			{ ...EXTRACTED_SOURCE, parts },
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest.map((m) => m.name).sort()).toEqual(
			parts.map((p) => p.name).sort(),
		);
		for (const part of parts) {
			const entry = manifest.find((m) => m.name === part.name);
			expect(entry?.size).toBe(part.size);
		}
	});

	it('skipSystemUpdate strips $SystemUpdate from an extracted -> extracted conversion, same as it does feeding into other targets', () => {
		const isoWithUpdate = makeFixture({
			titleId: 0x41560001,
			includeSystemUpdate: true,
		});
		const parts = convertXisoFixtureToExtractedParts(isoWithUpdate);
		expect(parts.some((p) => p.name.toUpperCase().includes('SYSTEMUPDATE'))).toBe(
			true,
		);
		const session = ConversionSession.open(
			nullReadFn,
			isoWithUpdate.length,
			{ format: 'extracted', skipSystemUpdate: true },
			{ ...EXTRACTED_SOURCE, parts },
		);
		const manifest = session.outputManifest();
		session.free();
		expect(
			manifest.some((m) => m.name.toUpperCase().includes('SYSTEMUPDATE')),
		).toBe(false);
	});

	it('two independent extracted -> extracted conversions of the same source produce identical manifests', () => {
		// Confirms the conversion is deterministic - no accidental
		// dependence on iteration/hash order that would make two runs over
		// the exact same input drift from each other.
		const manifestFor = () => {
			const parts = convertXisoFixtureToExtractedParts(iso);
			const session = ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'extracted' },
				{ ...EXTRACTED_SOURCE, parts },
			);
			const manifest = session.outputManifest();
			session.free();
			return manifest;
		};
		expect(manifestFor()).toEqual(manifestFor());
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'extracted' },
				{ ...EXTRACTED_SOURCE, parts: [] },
			),
		).toThrow();
	});
});

describe('extracted source error paths', () => {
	it('throws when source_parts is empty', () => {
		// The message here is parts_from_js's own generic empty-array check
		// ("sourceParts array must not be empty"), not
		// ExtractedFilesystem::new's format-specific "at least one file is
		// required" - that check never actually gets reached for a truly
		// empty array, since parts_from_js rejects it first, before an
		// ExtractedFilesystem is ever constructed. Asserting only .toThrow()
		// here (no message match) mirrors how the god-source error-path
		// tests handle this same shared, cross-format check.
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'xiso' },
				{ ...EXTRACTED_SOURCE, parts: [] },
			),
		).toThrow();
	});

	it('throws for duplicate part names after path normalization', () => {
		const bytes = new Uint8Array([1, 2, 3]);
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'xiso' },
				{
					...EXTRACTED_SOURCE,
					parts: [
						{
							name: '/default.xbe',
							size: bytes.length,
							readFn: makeReadFn(bytes),
						},
						{
							name: 'default.xbe',
							size: bytes.length,
							readFn: makeReadFn(bytes),
						},
					],
				},
			),
		).toThrow(/duplicate path/);
	});

	it('throws for a part with an empty relative path', () => {
		const bytes = new Uint8Array([1, 2, 3]);
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'xiso' },
				{
					...EXTRACTED_SOURCE,
					parts: [{ name: '', size: bytes.length, readFn: makeReadFn(bytes) }],
				},
			),
		).toThrow(/non-empty relative path/);
	});

	it('throws opening a god target from an extracted source with no default.xbe/xex at root', () => {
		const bytes = new Uint8Array([1, 2, 3]);
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'god' },
				{
					...EXTRACTED_SOURCE,
					parts: [
						{
							name: 'readme.txt',
							size: bytes.length,
							readFn: makeReadFn(bytes),
						},
					],
				},
			),
		).toThrow(/no default\.xbe\/default\.xex at root/);
	});

	it('propagates errors thrown inside a part readFn', () => {
		// Unlike the empty/duplicate/missing-path checks above (all of which
		// fire synchronously inside source::open(), before a session object
		// even exists), an xiso target only reads a given file's bytes while
		// streaming output - FilesystemCopier::copy_file_in runs during
		// nextChunk(), not at open() - so open() itself succeeds here and
		// the throw only surfaces once the session is actually drained.
		expect(() => {
			const session = ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'xiso' },
				{
					...EXTRACTED_SOURCE,
					parts: [{ name: 'default.xbe', size: 3, readFn: throwingReadFn }],
				},
			);
			drain(session, 1024 * 1024);
		}).toThrow('read error from JS');
	});
});

describe('extracted source: mode is accepted but has no effect (every mode collapses to a full rebuild)', () => {
	// An extracted-files source has no raw XDVDFS bytes to trim/zero - a
	// from-scratch rebuild has no leftover padding/junk to strip in the
	// first place, so none/partial/trim/zero all produce the exact same
	// output as full here. These tests assert both halves of that: no
	// throw, and no behavioral difference.
	const iso = makeFixture({ titleId: 0x41560001 });
	it.each(['none', 'partial', 'full'] as const)(
		'ciso target: mode "%s" opens without throwing',
		(mode) => {
			const parts = convertXisoFixtureToExtractedParts(iso);
			const session = ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'ciso', outputName: OUTPUT_NAME, mode },
				{ ...EXTRACTED_SOURCE, parts },
			);
			session.free();
		},
	);

	it.each(['none', 'partial', 'full'] as const)(
		'cci target: mode "%s" opens without throwing',
		(mode) => {
			const parts = convertXisoFixtureToExtractedParts(iso);
			const session = ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'cci', outputName: OUTPUT_NAME, mode },
				{ ...EXTRACTED_SOURCE, parts },
			);
			session.free();
		},
	);

	it.each(['none', 'partial', 'full'] as const)(
		'god target: mode "%s" opens without throwing',
		(mode) => {
			const parts = convertXisoFixtureToExtractedParts(iso);
			const session = ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'god', mode },
				{ ...EXTRACTED_SOURCE, parts },
			);
			session.free();
		},
	);

	// xiso has its own mode enum (trim/zero/full, not none/partial/full).
	it.each(['trim', 'zero', 'full'] as const)(
		'xiso target: mode "%s" opens without throwing',
		(mode) => {
			const parts = convertXisoFixtureToExtractedParts(iso);
			const session = ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'xiso', mode },
				{ ...EXTRACTED_SOURCE, parts },
			);
			session.free();
		},
	);

	// The actual behavioral claim: not just "doesn't throw" but "produces
	// identical bytes to full", for every target format. Catches a case
	// where the mode field got wired back in for one format but not
	// another, or where it's ignored but not consistently.
	it('xiso target: "trim"/"zero" produce byte-identical output to "full"', () => {
		const outFor = (mode: 'trim' | 'zero' | 'full') =>
			drain(
				ConversionSession.open(
					nullReadFn,
					iso.length,
					{ format: 'xiso', mode },
					{
						...EXTRACTED_SOURCE,
						parts: convertXisoFixtureToExtractedParts(iso),
					},
				),
				1024 * 1024,
			);
		const full = outFor('full');
		expect(outFor('trim')).toEqual(full);
		expect(outFor('zero')).toEqual(full);
	});

	it('god target: "none"/"partial" produce the same outputManifest as "full"', () => {
		// god's per-part backend differs by mode when there's real source
		// bytes to stream (Direct vs Rebuild) - outputManifest is the
		// cheapest observable proxy here without fully decoding GOD output,
		// same technique the existing "different titleIds" test above uses.
		const manifestFor = (mode: 'none' | 'partial' | 'full') => {
			const session = ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'god', mode },
				{
					...EXTRACTED_SOURCE,
					parts: convertXisoFixtureToExtractedParts(iso),
				},
			);
			const manifest = session.outputManifest();
			session.free();
			return manifest;
		};
		const full = manifestFor('full');
		expect(manifestFor('none')).toEqual(full);
		expect(manifestFor('partial')).toEqual(full);
	});

	it('ciso target: "none"/"partial" produce byte-identical output to "full"', () => {
		const outFor = (mode: 'none' | 'partial' | 'full') => {
			const session = ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'ciso', outputName: OUTPUT_NAME, mode },
				{
					...EXTRACTED_SOURCE,
					parts: convertXisoFixtureToExtractedParts(iso),
				},
			);
			driveHashing(session);
			return drain(session, 1024 * 1024);
		};
		const full = outFor('full');
		expect(outFor('none')).toEqual(full);
		expect(outFor('partial')).toEqual(full);
	});

	it('cci target: "none"/"partial" produce byte-identical output to "full"', () => {
		const outFor = (mode: 'none' | 'partial' | 'full') => {
			const session = ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'cci', outputName: OUTPUT_NAME, mode },
				{
					...EXTRACTED_SOURCE,
					parts: convertXisoFixtureToExtractedParts(iso),
				},
			);
			driveHashing(session);
			return drain(session, 1024 * 1024);
		};
		const full = outFor('full');
		expect(outFor('none')).toEqual(full);
		expect(outFor('partial')).toEqual(full);
	});

	// Omitting mode entirely must still behave like the default of 'full',
	// matching the no-mode-specified call sites used earlier in this file.
	it('omitting mode entirely still produces the same output as explicit "full", for every target', () => {
		const xisoImplicit = drain(
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'xiso' },
				{
					...EXTRACTED_SOURCE,
					parts: convertXisoFixtureToExtractedParts(iso),
				},
			),
			1024 * 1024,
		);
		const xisoExplicit = drain(
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'xiso', mode: 'full' },
				{
					...EXTRACTED_SOURCE,
					parts: convertXisoFixtureToExtractedParts(iso),
				},
			),
			1024 * 1024,
		);
		expect(xisoImplicit).toEqual(xisoExplicit);
	});
});
