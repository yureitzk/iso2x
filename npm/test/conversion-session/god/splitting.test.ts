import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { nameMap, scan, only } from '../../utils/read-fns.js';
import {
	convertXisoFixtureToGodParts,
	patchXexExecutionInfo,
} from '../../utils/session-helpers.js';
import { resolveBatchEntry, detectDirFormat } from '../../../dist/index.js';

beforeAll(async () => {
	await setupWasm();
});

/**
 * Converts a (tiny) xiso fixture to its real GOD `Data%04d` part(s) via
 * `convertXisoFixtureToGodParts`, and materializes them as a
 * `{ name: bytes }` map under an arbitrary `.data` parent directory,
 * renumbered from 0. Lets each test control a GOD candidate's full path
 * directly - the partitioner's grouping key
 * (`source::god_data_dir_and_index`) requires the parent directory's own
 * extension to be `.data`.
 *
 * Every fixture used below is small enough to stay under GOD's
 * part-size boundary, so this always returns exactly one `Data0000`
 * entry - but isn't written to assume that.
 */
function godFilesAt(
	dataDir: string,
	xiso: Uint8Array,
): Record<string, Uint8Array> {
	const { dataParts: parts } = convertXisoFixtureToGodParts(xiso);
	const files: Record<string, Uint8Array> = {};
	parts.forEach((part, i) => {
		files[`${dataDir}/Data${String(i).padStart(4, '0')}`] = part.readFn(
			0,
			part.size,
		);
	});
	return files;
}

/**
 * Like `godFilesAt`, but lets the caller control how each `DataNNNN`
 * part's filename is cased. The Xbox filesystem is case-insensitive,
 * so a real-world dump can just as easily land on disk as
 * `data0000` or `DATA0000` as the canonical `Data0000` -
 * `looks_god`'s matching needs to tolerate that.
 */
function godFilesAtCased(
	dataDir: string,
	xiso: Uint8Array,
	casePrefix: (index: number) => string,
): Record<string, Uint8Array> {
	const { dataParts: parts } = convertXisoFixtureToGodParts(xiso);
	const files: Record<string, Uint8Array> = {};
	parts.forEach((part, i) => {
		files[`${dataDir}/${casePrefix(i)}${String(i).padStart(4, '0')}`] =
			part.readFn(0, part.size);
	});
	return files;
}

describe('scanBatch (resolve_batch) - GOD candidate partitioning & the verification cost gate', () => {
	// A lone GOD-shaped folder costs zero verification, valid or not - see
	// `classify_god_candidates`'s doc comment in split_detect.rs.
	describe('cost gate: verification only runs once there is something to group against', () => {
		it('a single valid GOD folder resolves as an unverified GodFolder, not Standalone', async () => {
			const titleId = 0x47440101;
			const iso = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const files = godFilesAt('Solo.data', iso);
			const results = await scan(files);
			expect(results).toHaveLength(1);
			expect(results[0].kind).toBe('godFolder');
			if (results[0].kind !== 'godFolder') throw new Error('unreachable');
			expect(results[0].names.slice().sort()).toEqual(Object.keys(files).sort());
		});

		it('a single, structurally invalid (gappy) GOD folder still resolves as GodFolder, unverified - the gate never even looks at content when alone', async () => {
			// Naming-valid but content-invalid: no Data0000..N-1 run,
			// just a lone Data0007 - would fail
			// `god_candidate_is_contiguous` if verification ran.
			const files = { 'Broken.data/Data0007': new Uint8Array([1, 2, 3, 4]) };
			const results = await scan(files);
			expect(results).toHaveLength(1);
			expect(results[0].kind).toBe('godFolder');
			if (results[0].kind !== 'godFolder') throw new Error('unreachable');
			expect(results[0].names).toEqual(['Broken.data/Data0007']);
		});

		it('one GOD folder alongside an unrelated complete raw XISO still passes through unverified - only GOD-shaped candidates count toward the gate', async () => {
			const godTitleId = 0x47440102;
			const godIso = patchXexExecutionInfo(
				makeFixture({ titleId: godTitleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const isoTitleId = 0x47440103;
			const isoFile = makeFixture({ titleId: isoTitleId });
			const files = {
				...godFilesAt('Solo2.data', godIso),
				'Unrelated.iso': isoFile,
			};
			const results = await scan(files);
			expect(results).toHaveLength(2);
			const godResult = only(results, 'godFolder');
			expect(godResult.names).toEqual(['Solo2.data/Data0000']);
			const standalone = only(results, 'standalone');
			expect(standalone.titleId).toBe(isoTitleId.toString(16).toUpperCase());
		});

		it('two distinct GOD folders in the same batch flips the gate: both get fully verified, neither is reported as GodFolder', async () => {
			const titleA = 0x47440104;
			const titleB = 0x47440105;
			const isoA = patchXexExecutionInfo(
				makeFixture({ titleId: titleA, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const isoB = patchXexExecutionInfo(
				makeFixture({ titleId: titleB, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const files = {
				...godFilesAt('First.data', isoA),
				...godFilesAt('Second.data', isoB),
			};
			const results = await scan(files);
			expect(
				results
					.map((r) => r.kind)
					.slice()
					.sort(),
			).toEqual(['standalone', 'standalone']);
			const titleIds = results
				.map((r) => (r.kind === 'standalone' ? r.titleId : undefined))
				.slice()
				.sort();
			expect(titleIds).toEqual(
				[titleA, titleB]
					.map((t) => t.toString(16).toUpperCase())
					.slice()
					.sort(),
			);
		});
	});

	// Grouping key correctness: two GOD-shaped folders present (so the gate
	// above is satisfied and verification runs).
	describe('candidates are grouped by full parent-directory path, never leaf name or depth alone', () => {
		it('two candidates sharing the same leaf ".data" name but different full paths never merge, and neither corrupts the other', async () => {
			const titleA = 0x47440110;
			const titleB = 0x47440111;
			const isoA = patchXexExecutionInfo(
				makeFixture({ titleId: titleA, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const isoB = patchXexExecutionInfo(
				makeFixture({ titleId: titleB, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			// Same leaf directory name ("game.data") under two different
			// parents - a grouping key based on the leaf alone would
			// clobber these together.
			const files = {
				...godFilesAt('diskA/game.data', isoA),
				...godFilesAt('diskB/game.data', isoB),
			};
			const results = await scan(files);
			expect(
				results
					.map((r) => r.kind)
					.slice()
					.sort(),
			).toEqual(['standalone', 'standalone']);
			const titleIds = results
				.map((r) => (r.kind === 'standalone' ? r.titleId : undefined))
				.slice()
				.sort();
			expect(titleIds).toEqual(
				[titleA, titleB]
					.map((t) => t.toString(16).toUpperCase())
					.slice()
					.sort(),
			);
		});

		it('the same leaf ".data" name at different nesting depths is still two candidates, not one', async () => {
			const titleA = 0x47440112;
			const titleB = 0x47440113;
			const isoA = patchXexExecutionInfo(
				makeFixture({ titleId: titleA, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const isoB = patchXexExecutionInfo(
				makeFixture({ titleId: titleB, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const files = {
				// Shallow drop.
				...godFilesAt('media.data', isoA),
				// Same leaf name, nested several levels deep.
				...godFilesAt('very/deeply/nested/media.data', isoB),
			};
			const results = await scan(files);
			expect(
				results
					.map((r) => r.kind)
					.slice()
					.sort(),
			).toEqual(['standalone', 'standalone']);
			const titleIds = results
				.map((r) => (r.kind === 'standalone' ? r.titleId : undefined))
				.slice()
				.sort();
			expect(titleIds).toEqual(
				[titleA, titleB]
					.map((t) => t.toString(16).toUpperCase())
					.slice()
					.sort(),
			);
		});

		it('a GOD folder whose "DataNNNN" part filenames use non-standard casing is still recognized and correctly grouped', async () => {
			const titleId = 0x47440114;
			const iso = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			// A second, canonically-cased GOD candidate to satisfy the >1
			// gate and force real content verification to run, rather
			// than letting the lone-candidate cost gate wave the weirdly
			// cased folder through unverified.
			const otherTitleId = 0x47440115;
			const otherIso = patchXexExecutionInfo(
				makeFixture({ titleId: otherTitleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const files = {
				// "dATa0000" instead of "Data0000" - deliberately mixed
				// case, not just all-upper or all-lower, so this can't
				// pass by coincidentally matching a single alternate-case
				// literal.
				...godFilesAtCased('Weird.data', iso, () => 'dATa'),
				...godFilesAt('Other.data', otherIso),
			};
			const results = await scan(files);
			expect(
				results
					.map((r) => r.kind)
					.slice()
					.sort(),
			).toEqual(['standalone', 'standalone']);
			const titleIds = results
				.map((r) => (r.kind === 'standalone' ? r.titleId : undefined))
				.slice()
				.sort();
			expect(titleIds).toEqual(
				[titleId, otherTitleId]
					.map((t) => t.toString(16).toUpperCase())
					.slice()
					.sort(),
			);
		});
	});

	// Content verification, once the gate lets it run. Each case pairs the
	// broken candidate with a genuinely valid one to satisfy the ">1
	// candidate" gate, and asserts the valid one's result too.
	describe('content verification (once >1 GOD candidate makes it worth running)', () => {
		it('a numbering gap reports Unresolved with a gap/layout reason, and does not affect the valid sibling', async () => {
			const validTitle = 0x47440120;
			const validIso = patchXexExecutionInfo(
				makeFixture({ titleId: validTitle, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const files = {
				...godFilesAt('Valid.data', validIso),
				// Numbered "Data0001" - position 0 expects index 0, so
				// `god_candidate_is_contiguous` fails before any content
				// is read.
				'Gappy.data/Data0001': new Uint8Array([1, 2, 3, 4]),
			};
			const results = await scan(files);
			expect(results).toHaveLength(2);
			const valid = only(results, 'standalone');
			expect(valid.titleId).toBe(validTitle.toString(16).toUpperCase());
			const invalid = only(results, 'unresolved');
			expect(invalid.names).toEqual(['Gappy.data/Data0001']);
			expect(invalid.reason).toMatch(
				/gaps, wrong ordering, or an invalid layout/i,
			);
		});

		it('content that fails to parse as a GOD/XDVDFS volume at all reports Unresolved with the same gap/layout reason', async () => {
			const validTitle = 0x47440121;
			const validIso = patchXexExecutionInfo(
				makeFixture({ titleId: validTitle, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			// Contiguous naming passes `god_candidate_is_contiguous`,
			// but zeroed "content" fails when `GodSource::open`/
			// `probe_source_over` try to parse it.
			const validFiles = godFilesAt('Valid2.data', validIso);
			const zeroedSize = Object.values(validFiles)[0]!.length;
			const files = {
				...validFiles,
				'Zeroed.data/Data0000': new Uint8Array(zeroedSize),
			};
			const results = await scan(files);
			expect(results).toHaveLength(2);
			const valid = only(results, 'standalone');
			expect(valid.titleId).toBe(validTitle.toString(16).toUpperCase());
			const invalid = only(results, 'unresolved');
			expect(invalid.names).toEqual(['Zeroed.data/Data0000']);
			expect(invalid.reason).toMatch(
				/gaps, wrong ordering, or an invalid layout/i,
			);
		});

		// NOTE: the raw-XISO suite covers "valid XDVDFS image, no launch
		// executable" by corrupting default.xex's magic post-write. GOD
		// can't reuse that approach here: `GodSession::open` needs
		// `title_id`/`content_type` up front to name its own output
		// paths, so it likely already depends on parsing the launch
		// executable at conversion time - the same corruption would
		// probably fail the write itself before there'd be any Data
		// parts to test against. Left out rather than guessed at.

		it('a stray non-DataNNNN file inside a ".data" directory is ignored by the candidate, and reported separately as its own Unresolved entry', async () => {
			const titleId = 0x47440124;
			const iso = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			// A second GOD candidate just to satisfy the >1 gate.
			const otherTitleId = 0x47440125;
			const otherIso = patchXexExecutionInfo(
				makeFixture({ titleId: otherTitleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const files = {
				...godFilesAt('WithStray.data', iso),
				// Doesn't match "Data" + digits - excluded from the
				// candidate.
				'WithStray.data/thumbnail.png': new Uint8Array(16),
				...godFilesAt('Other.data', otherIso),
			};
			const results = await scan(files);
			expect(results).toHaveLength(3);
			const standalones = results.filter((r) => r.kind === 'standalone');
			expect(standalones).toHaveLength(2);
			const titleIds = standalones
				.map((r) => (r.kind === 'standalone' ? r.titleId : undefined))
				.slice()
				.sort();
			expect(titleIds).toEqual(
				[titleId, otherTitleId]
					.map((t) => t.toString(16).toUpperCase())
					.slice()
					.sort(),
			);
			const stray = only(results, 'unresolved');
			expect(stray.names).toEqual(['WithStray.data/thumbnail.png']);
		});
	});

	// `resolve_complete_images` grouping semantics, exercised through GOD
	// sources rather than raw XISOs.
	describe('multi-disc packaging semantics for GOD sources', () => {
		it('two GOD folders sharing titleId+discCount group into one MultiDiscSet', async () => {
			const titleId = 0x47440130;
			const disc1 = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 2, mediaId: 0x1 },
			);
			const disc2 = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 2, discCount: 2, mediaId: 0x2 },
			);
			const files = {
				...godFilesAt('Set/Disc1.data', disc1),
				...godFilesAt('Set/Disc2.data', disc2),
			};
			const results = await scan(files);
			expect(results).toHaveLength(1);
			const set = only(results, 'multiDiscSet');
			expect(set.discCount).toBe(2);
			expect(set.discs).toHaveLength(2);
			expect(
				set.discs
					.map((d) => d.discNumber)
					.slice()
					.sort(),
			).toEqual([1, 2]);
			expect(
				set.discs
					.map((d) => d.name)
					.slice()
					.sort(),
			).toEqual(['Set/Disc1.data/Data0000', 'Set/Disc2.data/Data0000']);
		});

		// A disc set must ship every disc in one shared raw container -
		// see ContainerShape's doc comment in split_detect.rs. A GOD
		// folder and a plain raw-XISO file sharing titleId+discCount are
		// therefore never siblings, even though they agree on every other
		// field: `resolve_complete_images` groups (and duplicate-disc-
		// checks) each container shape independently now, so the two
		// GOD-shaped discs below still group with each other while the
		// raw-XISO disc resolves on its own instead of joining them.
		it('a GOD folder and a plain raw-XISO file sharing titleId+discCount never group into the same MultiDiscSet - container shape must match too', async () => {
			const titleId = 0x47440131;
			const godDisc1 = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 3, mediaId: 0x1 },
			);
			const godDisc2 = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 2, discCount: 3, mediaId: 0x2 },
			);
			const rawDisc3 = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 3, discCount: 3, mediaId: 0x3 },
			);
			const files = {
				...godFilesAt('MixedSet/Disc1.data', godDisc1),
				...godFilesAt('MixedSet/Disc2.data', godDisc2),
				'MixedSet (Disc 3).iso': rawDisc3,
			};
			const results = await scan(files);
			// The two GOD discs (their own shape group of 2) plus the lone
			// raw-XISO disc (its own shape group of 1) - never one set of 3.
			expect(results).toHaveLength(2);

			const set = only(results, 'multiDiscSet');
			expect(set.discs).toHaveLength(2);
			expect(
				set.discs
					.map((d) => d.discNumber)
					.slice()
					.sort(),
			).toEqual([1, 2]);
			const godNames = set.discs
				.map((d) => d.name)
				.slice()
				.sort();
			expect(godNames).toEqual([
				'MixedSet/Disc1.data/Data0000',
				'MixedSet/Disc2.data/Data0000',
			]);

			// The raw-XISO disc, being a different container shape, falls
			// out of that grouping entirely and resolves standalone.
			const standalone = only(results, 'standalone');
			expect(standalone.names).toEqual(['MixedSet (Disc 3).iso']);
		});

		it('two GOD folders claiming the same disc_number of a would-be set are reported Unresolved, not silently formed into a set', async () => {
			const titleId = 0x47440132;
			const claimA = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 2, mediaId: 0x1 },
			);
			const claimB = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				// Also claims disc 1, not disc 2 - a duplicate, not a sibling.
				{ discNumber: 1, discCount: 2, mediaId: 0x9 },
			);
			const files = {
				...godFilesAt('DupA.data', claimA),
				...godFilesAt('DupB.data', claimB),
			};
			const results = await scan(files);
			expect(results).toHaveLength(2);
			for (const r of results) {
				expect(r.kind).toBe('unresolved');
				if (r.kind !== 'unresolved') throw new Error('unreachable');
				expect(r.reason).toMatch(/multiple sources claim disc 1 of a 2-disc/i);
			}
			const names = results
				.map((r) => (r.kind === 'unresolved' ? r.names[0] : undefined))
				.slice()
				.sort();
			expect(names).toEqual(['DupA.data/Data0000', 'DupB.data/Data0000']);
		});

		// looks_god (source.rs) inspects only the immediate parent segment for
		// the ".data" match itself - renaming that segment's parent, or the
		// folders above it, doesn't matter. It does separately cap total depth
		// at parts.len() <= 4 (documented on partition_god_candidates/
		// GodCandidate as a deliberate difference from scanBatch's own,
		// depth-unlimited grouping) - this stays within that cap on purpose,
		// since testing past it is a different, already-known constraint, not
		// this claim.
		it('a GOD folder is still detected regardless of what its own parent path is named', async () => {
			const titleId = 0x47440199;
			const iso = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const files = godFilesAt('Some/Renamed.data', iso);
			const results = await scan(files);
			expect(results).toHaveLength(1);
			expect(results[0].kind).toBe('godFolder');
			if (results[0].kind !== 'godFolder') throw new Error('unreachable');
			expect(results[0].names.slice().sort()).toEqual(Object.keys(files).sort());
		});

		// Same claim against detectDirFormat directly - the function
		// partitionDirEntries()/resolveDroppedSource() actually call in the
		// live app to classify a dropped folder. Bounded at exactly the
		// documented parts.len() <= 4 cap ("Some/Renamed.data/Data0000" is 3
		// segments, well inside it) - this is about the parent name, not
		// about probing the cap's edge.
		it('detectDirFormat recognizes a GOD folder through a renamed parent path', () => {
			const titleId = 0x4744019a;
			const iso = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const files = godFilesAt('Some/Renamed.data', iso);
			expect(detectDirFormat(Object.keys(files))).toBe('god');
		});

		it('two byte-identical GOD folders (accidental duplicate drop) each resolve standalone, not merged', async () => {
			const titleId = 0x47440133;
			const iso = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 1, mediaId: 0x1 },
			);
			const files = {
				...godFilesAt('Copy1.data', iso),
				...godFilesAt('Copy2.data', iso.slice()),
			};
			const results = await scan(files);
			expect(results).toHaveLength(2);
			for (const r of results) {
				expect(r.kind).toBe('standalone');
				if (r.kind !== 'standalone') throw new Error('unreachable');
				expect(r.titleId).toBe(titleId.toString(16).toUpperCase());
			}
		});
	});
});

// `resolveBatchEntry` (and the `resolveArbitraryXisoSplit` it's built on)
// stays entirely GOD-oblivious by design - see `resolve_arbitrary_xiso_split_over`
// in split_detect.rs, which always passes an empty GOD-candidate list.
describe('resolveBatchEntry never resolves a GOD folder - GOD grouping is scanBatch-only', () => {
	it('a lone Data0000 part resolves through the plain single-file fallback, not as a "dir"/god source', async () => {
		const titleId = 0x47440140;
		const iso = patchXexExecutionInfo(
			makeFixture({ titleId, platform: 'x360' }),
			{ discNumber: 1, discCount: 1, mediaId: 0x1 },
		);
		const files = godFilesAt('Lone.data', iso);
		const accessor = nameMap(files);
		const resolved = await resolveBatchEntry(Object.keys(files), accessor);
		// Falls through named CCI/CISO detection and the raw-XISO search
		// (neither sees a GOD candidate) to the plain single-file
		// fallback, defaulting to 'xiso' since a Data0000 part carries
		// no CISO/CCI/STFS/ZAR magic.
		expect(resolved.kind).toBe('file');
		if (resolved.kind !== 'file') throw new Error('unreachable');
		expect(resolved.format).toBe('xiso');
		expect(resolved.fileSize).toBe(accessor.size('Lone.data/Data0000'));
	});
});
