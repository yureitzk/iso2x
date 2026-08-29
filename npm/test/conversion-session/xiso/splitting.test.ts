import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import {
	DEFAULT_XBE_DECLARED_SIZE,
	makeFixture,
} from '../../utils/fixtures/xsf.js';
import { ConversionSession, resolveBatchEntry } from '../../../dist/index.js';
import {
	checkIsoCompleteness,
	verifySplitCandidate,
	resolveArbitraryXisoSplit,
} from '../../../dist/detect-advanced.js';
import { makeReadFn, nameMap, scan } from '../../utils/read-fns.js';
import {
	drain,
	makePart,
	convertXisoFixtureToBytes,
	patchXexExecutionInfo,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(async () => {
	await setupWasm();
});

// Split file names are derived from this ("<outputName>.1.xiso.iso",
// "<outputName>.2.xiso.iso", ...).
const OUTPUT_NAME = 'test';

describe('ConversionSession(xiso) splitting', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);

	it('opens without throwing when split is true and outputName is given', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'xiso',
				split: true,
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		session.free();
	});

	// Once split is turned on, xiso always reports an entry name, same as
	// ciso/extracted - for output under the split threshold, that's the
	// same "<outputName>.1.xiso.iso" name for every chunk.
	it('currentEntryName is "<outputName>.1.xiso.iso" for every chunk, for output that never crosses the split threshold', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'xiso',
				split: true,
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		let chunkCount = 0;
		while (!session.isDone()) {
			session.nextChunk(2048);
			expect(session.currentEntryName()).toBe(`${OUTPUT_NAME}.1.xiso.iso`);
			chunkCount++;
		}
		session.free();
		expect(chunkCount).toBeGreaterThan(0);
	});

	it('outputManifest has exactly one entry immediately after open(), with no hashNextPart() step needed', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'xiso',
				split: true,
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest).toHaveLength(1);
		expect(manifest[0].name).toBe(`${OUTPUT_NAME}.1.xiso.iso`);
	});

	it('outputManifest size matches totalUnits() * 2048 for output under the split threshold', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'xiso',
				split: true,
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		const totalBytes = session.totalUnits() * 2048;
		session.free();
		expect(manifest[0].size).toBe(totalBytes);
	});

	it('outputManifest size matches the drained byte count', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'xiso',
				split: true,
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		const out = drain(session, 64 * 2048);
		expect(manifest[0].size).toBe(out.length);
	});

	it('outputManifest entry names are derived from outputName', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'xiso',
				split: true,
				outputName: 'Halo 3',
			},
			XISO_SOURCE,
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest[0].name).toBe('Halo 3.1.xiso.iso');
	});

	it('splitting does not affect drained byte content - naming/manifest are metadata-only, not a transform', () => {
		const unsplit = drain(
			ConversionSession.open(readFn, iso.length, { format: 'xiso' }, XISO_SOURCE),
			64 * 2048,
		);
		const split = drain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'xiso',
					split: true,
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
			64 * 2048,
		);
		expect(split).toEqual(unsplit);
	});

	it('chunk size does not affect final output when split is on either', () => {
		const out1 = drain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'xiso',
					split: true,
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
			1,
		);
		const outAll = drain(
			ConversionSession.open(
				readFn,
				iso.length,
				{
					format: 'xiso',
					split: true,
					outputName: OUTPUT_NAME,
				},
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		expect(out1).toEqual(outAll);
	});

	// Splitting is orthogonal to mode - it clamps by sector count
	// regardless of which backend produced the sectors. Spot-checking
	// trim mode is enough to catch a regression that ties splitting to
	// one specific backend.
	it('works together with trim mode', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'xiso',
				mode: 'trim',
				split: true,
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		let chunkCount = 0;
		while (!session.isDone()) {
			session.nextChunk(2048);
			expect(session.currentEntryName()).toBe(`${OUTPUT_NAME}.1.xiso.iso`);
			chunkCount++;
		}
		session.free();
		expect(chunkCount).toBeGreaterThan(0);
	});
});

/**
 * Corrupts the launch executable's magic bytes in place, leaving the
 * XDVDFS volume descriptor/directory table (and completeness) untouched:
 * the image still probes complete, but `TitleInfo::from_image` can't
 * parse a launch executable back out of it. The stub always sits at
 * sector 0x22 (see fixtures/xsf.ts), regardless of `rootOffset`.
 */
function corruptLaunchExecutableMagic(
	iso: Uint8Array,
	rootOffset = 0,
): Uint8Array {
	const corrupted = iso.slice();
	const EXE = rootOffset + 0x22 * 0x800;
	corrupted.set([0, 0, 0, 0], EXE);
	return corrupted;
}

// Read side: detecting an already-split source under arbitrary filenames
// (no ".1."/".2." naming convention required). Unlike CCI/CISO, a raw
// XISO split has no self-describing header on parts 2+, so this is the
// only format that needs arbitrary-name content verification.
describe('arbitrary-filename raw XISO split detection', () => {
	let isoA: Uint8Array;
	let isoB: Uint8Array;
	/**
	 * Where a genuine part-1/part-2 cut can land while still guaranteeing
	 * (a) part 1 alone reads back as incomplete, and (b) default.xbe's
	 * magic bytes land in part 2+, so verifySplitCandidate has something
	 * real to spot-check.
	 *
	 * The volume descriptor and root directory table sit near the *end*
	 * of this fixture, not near byte 0, so a naive fraction of
	 * maxUsedPrefixSize would land before the header even starts.
	 * Anchoring on default.xbe's own start lands the cut exactly on
	 * `boundary_in_volume`, also exercising the "magic bytes start
	 * exactly on the boundary" case.
	 */
	let rootOffset: number;
	let maxUsedPrefixSize: number;
	/** Cut at the default.xbe entry's own start. */
	let cut: number;

	beforeAll(async () => {
		isoA = makeFixture({ titleId: 0x41560010 });
		isoB = makeFixture({ titleId: 0x41560011 });

		const info = checkIsoCompleteness(makeReadFn(isoA), isoA.length);
		expect(info).toBeDefined();
		expect(info!.isComplete).toBe(true);
		rootOffset = info!.rootOffset;
		maxUsedPrefixSize = info!.maxUsedPrefixSize;
		cut = rootOffset + maxUsedPrefixSize - DEFAULT_XBE_DECLARED_SIZE;
	});

	describe('checkIsoCompleteness', () => {
		it('reports a whole image as complete', async () => {
			const info = checkIsoCompleteness(makeReadFn(isoA), isoA.length);
			expect(info?.isComplete).toBe(true);
		});

		it('reports a truncated header (missing continuation bytes) as incomplete', async () => {
			const part1 = isoA.slice(0, cut);
			const info = checkIsoCompleteness(makeReadFn(part1), part1.length);
			expect(info).toBeDefined();
			expect(info!.isComplete).toBe(false);
			expect(info!.rootOffset).toBe(rootOffset);
		});

		it('returns undefined for a headerless continuation fragment', async () => {
			const part2 = isoA.slice(cut);
			const info = checkIsoCompleteness(makeReadFn(part2), part2.length);
			expect(info).toBeUndefined();
		});
	});

	describe('verifySplitCandidate', () => {
		it('accepts the correct two-part ordering, spot-checking the entry whose magic starts exactly on the split boundary', async () => {
			const part1 = makePart('a', isoA.slice(0, cut));
			const part2 = makePart('b', isoA.slice(cut));
			const verify = verifySplitCandidate([part1, part2]);
			expect(verify.ok).toBe(true);
			// `cut` lands exactly on boundary_in_volume - verifiable_entries_past
			// only excludes strictly-less-than, so default.xbe must still
			// show up here as checked, not silently skipped as "in part 1".
			expect(verify.checkedEntries).toHaveLength(1);
			expect(verify.checkedEntries[0].matched).toBe(true);
			expect(verify.reason).toBeUndefined();
		});

		it('rejects the reversed ordering of a genuine two-part split', async () => {
			const part1 = makePart('a', isoA.slice(0, cut));
			const part2 = makePart('b', isoA.slice(cut));
			// Swapped: the concatenated stream now starts with raw
			// continuation bytes, so either the header probe fails, or
			// the spot-check won't find the expected magic in place.
			const verify = verifySplitCandidate([part2, part1]);
			expect(verify.ok).toBe(false);
		});

		it('rejects pairing part 1 with unrelated data at the same offset', async () => {
			const part1 = makePart('a', isoA.slice(0, cut));
			// A second makeFixture() output isn't a fair stand-in for an
			// unrelated file here - the generator always places the launch
			// executable at the same sector with the same magic, so two
			// same-platform fixtures would trivially agree. Zero-filled
			// bytes model a genuinely unrelated file deterministically.
			const unrelatedTail = makePart('b', new Uint8Array(isoA.length - cut));
			const verify = verifySplitCandidate([part1, unrelatedTail]);
			expect(verify.ok).toBe(false);
		});

		// `verify_ordering` runs three checks before any spot-check: (1)
		// header/directory table parses, (2) combined size covers what
		// the directory table references, (3) part 1 alone is at least
		// as large as the detected root offset. Each test below isolates
		// one, plus the "nothing left to spot-check" case.
		describe('boundary & math checks', () => {
			it('fails when the supplied part 1 is smaller than the detected root offset', async () => {
				// One of the four fixed offsets `detect()` probes (Xsf/Xgd2/
				// Xgd1/Xgd3) - the smallest nonzero one, to keep this
				// fixture's allocation reasonable.
				const ROOT_OFFSET = 0x2080000; // Xgd3
				const offsetIso = makeFixture({
					titleId: 0x41560020,
					rootOffset: ROOT_OFFSET,
				});
				// verify_ordering probes the combined multi-part reader
				// for the header, not part 1 alone - so a small part 1
				// still reaches that probe before tripping the dedicated
				// part1-vs-root-offset check.
				const part1 = makePart('a', offsetIso.slice(0, 4096));
				const part2 = makePart('b', offsetIso.slice(4096));
				const verify = verifySplitCandidate([part1, part2]);
				expect(verify.ok).toBe(false);
				expect(verify.checkedEntries).toEqual([]);
				expect(verify.reason).toMatch(
					/part 1 is smaller than the detected root offset/i,
				);
			});

			it("fails when the parts combined fall short of the directory table's referenced data", async () => {
				// Part 1 alone already satisfies "part 1 >= root offset" (cut
				// sits well past it), but pairing it with only a sliver of
				// the remaining bytes - far short of maxUsedPrefixSize -
				// trips the total-size check instead.
				const part1 = makePart('a', isoA.slice(0, cut));
				const shortTail = makePart('b', isoA.slice(cut, cut + 16));
				const verify = verifySplitCandidate([part1, shortTail]);
				expect(verify.ok).toBe(false);
				expect(verify.checkedEntries).toEqual([]); // early-exit, no spot-checks ever run
				expect(verify.reason).toMatch(/directory table references data up to/i);
			});

			it('passes on size/header alone, with an explanatory reason, when no executable lands past part 1', async () => {
				// Cut *after* the default.xbe entry's own end
				// (maxUsedPrefixSize) but before the fixture's trailing
				// padding runs out - a legitimate two-part split where every
				// checkable entry already sits entirely inside part 1.
				const pastExeEnd = rootOffset + maxUsedPrefixSize;
				const part1 = makePart('a', isoA.slice(0, pastExeEnd));
				const part2 = makePart('b', isoA.slice(pastExeEnd));
				expect(part2.size).toBeGreaterThan(0); // still a real second part
				const verify = verifySplitCandidate([part1, part2]);
				expect(verify.ok).toBe(true);
				expect(verify.checkedEntries).toEqual([]);
				expect(verify.reason).toMatch(/no executable entries land past part 1/i);
			});
		});
	});

	describe('resolveArbitraryXisoSplit', () => {
		it('reassembles a genuine split under arbitrary (non ".1."/".2.") filenames', async () => {
			const files = {
				'disc-fragment-alpha.bin': isoA.slice(0, cut),
				'disc-fragment-beta.bin': isoA.slice(cut),
			};
			const result = await resolveArbitraryXisoSplit(
				Object.keys(files),
				nameMap(files),
			);
			expect(result).not.toBeNull();
			expect(result!.parts).toEqual([
				'disc-fragment-alpha.bin',
				'disc-fragment-beta.bin',
			]);
			expect(result!.verify.ok).toBe(true);
		});

		it('finds the correct order for a three-part split regardless of input order', async () => {
			// Both intermediate cuts must land at/after default.xbe's own
			// start so header.bin alone still parses as a truncated-but-
			// valid header; cut2 just needs to fall strictly between cut1
			// and isoA.length.
			const cut1 = cut;
			const cut2 =
				rootOffset + maxUsedPrefixSize - Math.floor(DEFAULT_XBE_DECLARED_SIZE / 2);
			const files = {
				// Deliberately not "1"/"2"/"3", and not alphabetical - the
				// point is this can't be solved by sorting the names.
				'zzz-tail.bin': isoA.slice(cut2),
				'header.bin': isoA.slice(0, cut1),
				'mid-chunk.bin': isoA.slice(cut1, cut2),
			};
			const result = await resolveArbitraryXisoSplit(
				Object.keys(files),
				nameMap(files),
			);
			expect(result).not.toBeNull();
			expect(result!.parts).toEqual([
				'header.bin',
				'mid-chunk.bin',
				'zzz-tail.bin',
			]);
		});

		it('does not merge two independently complete discs', async () => {
			// Two whole, valid, unrelated images. Neither is a truncated
			// header, so there's no headerCandidate at all - this must
			// return null, never attempt to glue them together - siblings on
			// the same disc set are never treated as split fragments.
			const files = {
				'Game (Disc 1).iso': isoA,
				'Game (Disc 2).iso': isoB,
			};
			const result = await resolveArbitraryXisoSplit(
				Object.keys(files),
				nameMap(files),
			);
			expect(result).toBeNull();
		});

		it('returns null when a lone truncated header has no continuation to pair with', async () => {
			const files = { 'part1-only.bin': isoA.slice(0, cut) };
			const result = await resolveArbitraryXisoSplit(
				Object.keys(files),
				nameMap(files),
			);
			expect(result).toBeNull();
		});

		it('returns null (ambiguous) when two different truncated headers appear in the same batch', async () => {
			const files = {
				'a-part1.bin': isoA.slice(0, cut),
				'a-part2.bin': isoA.slice(cut),
				'b-part1.bin': isoB.slice(0, cut),
				'b-part2.bin': isoB.slice(cut),
			};
			// Two headerCandidates in one batch -> bail out rather than
			// guessing which continuation fragment belongs to which.
			const result = await resolveArbitraryXisoSplit(
				Object.keys(files),
				nameMap(files),
			);
			expect(result).toBeNull();
		});
	});

	describe('resolveBatchEntry integration', () => {
		it('an arbitrarily-named split resolves to a dir source that reads back identically to the unsplit image', async () => {
			const files = {
				'fragment-one': isoA.slice(0, cut),
				'fragment-two': isoA.slice(cut),
			};
			const accessor = nameMap(files);
			const resolved = await resolveBatchEntry(Object.keys(files), accessor);

			expect(resolved.kind).toBe('dir');
			if (resolved.kind !== 'dir') throw new Error('unreachable');
			expect(resolved.format).toBe('xiso');
			expect(resolved.parts.map((p) => p.name)).toEqual([
				'fragment-one',
				'fragment-two',
			]);

			const session = ConversionSession.open(
				accessor.readFn('fragment-one'), // fallback readFn/size - unused once sourceParts is given
				accessor.size('fragment-one'),
				{ format: 'xiso' },
				{ source: XISO_SOURCE.source, parts: resolved.parts },
			);
			const out = drain(session, 64 * 2048);
			expect(out).toEqual(
				drain(
					ConversionSession.open(
						makeReadFn(isoA),
						isoA.length,
						{ format: 'xiso' },
						XISO_SOURCE,
					),
					64 * 2048,
				),
			);
		});

		it('two independently valid discs each resolve as their own standalone source, not a merged split', async () => {
			const files = {
				'Game (Disc 1).iso': isoA,
				'Game (Disc 2).iso': isoB,
			};
			const entries = Object.keys(files);

			// First call resolves disc 1 alone - a real caller's loop would
			// remove it from `entries` and call again for disc 2.
			const resolved = await resolveBatchEntry(entries, nameMap(files));
			expect(resolved.kind).toBe('file');
			if (resolved.kind !== 'file') throw new Error('unreachable');
			expect(resolved.format).toBe('xiso');
			expect(resolved.fileSize).toBe(isoA.length);
		});
	});

	// `invalidKind` is the caller's only reliable signal for telling a
	// dead-end `Invalid` result from a recoverable one. `mismatch` is
	// covered separately by the CCI/CISO named-split tests; this suite
	// covers the two raw-XISO variants.
	describe('resolveBatchEntry invalidKind (raw XISO)', () => {
		it('tags a single truncated header with a non-verifying continuation as unresolvedOrdering', async () => {
			// One real header (isoA, truncated at `cut`) paired with a
			// continuation fragment of unrelated data - the shapes are both
			// right, so this reaches find_raw_split, but the spot-check
			// never matches, so no ordering verifies. Zero-filled, not a
			// second makeFixture() output, since two same-platform
			// fixtures would trivially verify against each other.
			const files = {
				'isoA-header.bin': isoA.slice(0, cut),
				'unrelated-tail.bin': new Uint8Array(isoA.length - cut),
			};
			const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));
			expect(resolved.kind).toBe('invalid');
			if (resolved.kind !== 'invalid') throw new Error('unreachable');
			expect(resolved.invalidKind).toBe('unresolvedOrdering');
			expect(resolved.names.slice().sort()).toEqual(
				['isoA-header.bin', 'unrelated-tail.bin'].sort(),
			);
		});

		it('tags two distinct truncated headers in one batch as ambiguousHeaders', async () => {
			// Two different images, each truncated at `cut` - both look like
			// a genuine part-1 header/directory table, and nothing (naming,
			// content) says which one a caller should try to complete first.
			const files = {
				'header-a.bin': isoA.slice(0, cut),
				'header-b.bin': isoB.slice(0, cut),
			};
			const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));
			expect(resolved.kind).toBe('invalid');
			if (resolved.kind !== 'invalid') throw new Error('unreachable');
			expect(resolved.invalidKind).toBe('ambiguousHeaders');
			expect(resolved.names.slice().sort()).toEqual(
				['header-a.bin', 'header-b.bin'].sort(),
			);
		});
	});

	// A different edge case from "two independently valid discs": there, the
	// two files are genuinely different (different titleId). Here, they're
	// byte-for-byte the same file dropped twice (e.g. an accidental duplicate,
	// or a renamed copy). Worth covering explicitly since it exercises the
	// same "don't glue independently-complete images together" guarantee
	// under the extra wrinkle of the two candidates being indistinguishable
	// by content.
	describe('duplicate/identical images', () => {
		it('two byte-identical images are each independently complete, not a split pair', async () => {
			const files = {
				'game.iso': isoA,
				'game (copy).iso': isoA.slice(), // fresh copy, same bytes
			};
			const result = await resolveArbitraryXisoSplit(
				Object.keys(files),
				nameMap(files),
			);
			// Both are independently valid/complete (isComplete: true) - no
			// headerCandidate exists at all, so this must never try to glue
			// two copies of the same disc together.
			expect(result).toBeNull();
		});

		it('resolveBatchEntry resolves each identical copy as its own standalone source', async () => {
			const files = {
				'game.iso': isoA,
				'game (copy).iso': isoA.slice(),
			};
			const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));
			expect(resolved.kind).toBe('file');
			if (resolved.kind !== 'file') throw new Error('unreachable');
			expect(resolved.format).toBe('xiso');
			expect(resolved.fileSize).toBe(isoA.length);
		});
	});
});

// Whole-batch classification via scanBatch. Unlike the
// resolveArbitraryXisoSplit/resolveBatchEntry tests above (which resolve
// one candidate/entry at a time), scanBatch classifies every file in a
// batch dir in one pass: independently complete images (grouped into
// MultiDiscSets), raw-split fragment sets, and anything left unresolved.
describe('scanBatch (resolve_batch) - classification & permutation search', () => {
	let isoA: Uint8Array;
	let isoB: Uint8Array;
	let rootOffset: number;
	let maxUsedPrefixSize: number;
	let cut: number;

	beforeAll(async () => {
		isoA = makeFixture({ titleId: 0x41560030 });
		isoB = makeFixture({ titleId: 0x41560031 });
		const info = checkIsoCompleteness(makeReadFn(isoA), isoA.length);
		rootOffset = info!.rootOffset;
		maxUsedPrefixSize = info!.maxUsedPrefixSize;
		cut = rootOffset + maxUsedPrefixSize - DEFAULT_XBE_DECLARED_SIZE;
	});

	// Batch classification & permutations
	describe('header/continuation classification', () => {
		it('reports Unresolved for two ambiguous truncated-header candidates sharing a batch', async () => {
			const files = {
				'a-part1.bin': isoA.slice(0, cut),
				'a-part2.bin': isoA.slice(cut),
				'b-part1.bin': isoB.slice(0, cut),
				'b-part2.bin': isoB.slice(cut),
			};
			const results = await scan(files);
			expect(results).toHaveLength(1);
			expect(results[0].kind).toBe('unresolved');
			if (results[0].kind !== 'unresolved') throw new Error('unreachable');
			expect(results[0].names.slice().sort()).toEqual(Object.keys(files).sort());
			expect(results[0].reason).toMatch(
				/multiple ambiguous truncated-header candidates/i,
			);
		});

		it('reports Unresolved for a truncated header with no continuation fragments in the batch', async () => {
			const files = { 'lonely-header.bin': isoA.slice(0, cut) };
			const results = await scan(files);
			expect(results).toHaveLength(1);
			expect(results[0].kind).toBe('unresolved');
			if (results[0].kind !== 'unresolved') throw new Error('unreachable');
			expect(results[0].names).toEqual(['lonely-header.bin']);
			expect(results[0].reason).toMatch(/no continuation fragments/i);
		});

		it('reports Unresolved for continuation fragments with no matching header in the batch', async () => {
			// A bare continuation fragment has no magic of its own, so
			// `partition_xiso_candidates`'s `detect()` call defaults it to
			// FileType::Xiso (its fallback for anything that doesn't match a
			// more specific format) - it still lands in the xiso-candidate
			// bucket, just with nothing to pair it with.
			const files = { 'orphan-continuation.bin': isoA.slice(cut) };
			const results = await scan(files);
			expect(results).toHaveLength(1);
			expect(results[0].kind).toBe('unresolved');
			if (results[0].kind !== 'unresolved') throw new Error('unreachable');
			expect(results[0].names).toEqual(['orphan-continuation.bin']);
			expect(results[0].reason).toMatch(/no matching xdvdfs header/i);
		});

		it('reports Invalid, not File, for a batch with multiple ambiguous truncated-header candidates', async () => {
			// Two different discs, each split without the ".1."/".2." naming
			// convention.
			const files = {
				'a-part1.bin': isoA.slice(0, cut),
				'a-part2.bin': isoA.slice(cut),
				'b-part1.bin': isoB.slice(0, cut),
				'b-part2.bin': isoB.slice(cut),
			};
			const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));

			expect(resolved.kind).toBe('invalid');
			if (resolved.kind !== 'invalid') throw new Error('unreachable');
			expect(resolved.names.slice().sort()).toEqual(Object.keys(files).sort());
			expect(resolved.reason).toMatch(
				/multiple ambiguous truncated-header candidates/i,
			);
		});

		it('reports Invalid, not File, when a header pairs with a continuation but no ordering verifies', async () => {
			// One header, one continuation - but the continuation's XBE
			// magic (https://xboxdevwiki.net/Xbe) is corrupted, so no
			// ordering passes verifySplitCandidate.
			const part1 = isoA.slice(0, cut);
			const part2 = isoA.slice(cut).slice();
			part2.set([0, 0, 0, 0], 0);
			const files = { 'header.bin': part1, 'continuation.bin': part2 };

			const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));

			expect(resolved.kind).toBe('invalid');
			if (resolved.kind !== 'invalid') throw new Error('unreachable');
			expect(resolved.names.slice().sort()).toEqual(Object.keys(files).sort());
			expect(resolved.reason).toMatch(/no ordering of these parts verified/i);
		});

		it('finds the correct ordering for a three-part split fed in the wrong physical order', async () => {
			const cut1 = cut;
			const cut2 =
				rootOffset + maxUsedPrefixSize - Math.floor(DEFAULT_XBE_DECLARED_SIZE / 2);
			// Deliberately out of physical order: header, then part 3, then
			// part 2 - depth_first `permutations` must still brute-force its
			// way to the one ordering that verifies.
			const files = {
				'p1.bin': isoA.slice(0, cut1),
				'p3.bin': isoA.slice(cut2),
				'p2.bin': isoA.slice(cut1, cut2),
			};
			const results = await scan(files);
			expect(results).toHaveLength(1);
			expect(results[0].kind).toBe('rawSplit');
			if (results[0].kind !== 'rawSplit') throw new Error('unreachable');
			expect(results[0].parts).toEqual(['p1.bin', 'p2.bin', 'p3.bin']);
			expect(results[0].verify.ok).toBe(true);
		});
	});

	// Every fixture above cuts part 1 near the end of the image, so the
	// directory-tree walk always completes and is_complete is what goes
	// false. Real-world splits aren't that considerate - the cut can land
	// mid-directory-table, before any file data. That case must still be
	// recognized as a truncated XDVDFS *header* candidate, not
	// miscategorized as "not XDVDFS at all" because the walk errors out.
	describe('truncation inside the directory table itself (not just trailing file data)', () => {
		let iso: Uint8Array;
		let rootOffset: number;
		let rootDirTableOffset: number;

		beforeAll(() => {
			iso = makeFixture({ titleId: 0x41560060 });
			const info = checkIsoCompleteness(makeReadFn(iso), iso.length);
			rootOffset = info!.rootOffset;
			rootDirTableOffset = info!.rootDirectoryTableOffset;
		});

		// rootDirTableOffset (not rootOffset - that's just the partition base,
		// 0 for a plain XISO) is where the root directory table's own data
		// starts. +8 lands inside its very first dirent, before any full
		// entry - let alone any file data - has been read.
		function truncatedParts() {
			const cut = rootDirTableOffset + 8;
			return { part1: iso.slice(0, cut), part2: iso.slice(cut) };
		}

		it('checkIsoCompleteness recognizes part 1 as an incomplete XDVDFS header, not "not XDVDFS"', () => {
			const { part1 } = truncatedParts();
			const info = checkIsoCompleteness(makeReadFn(part1), part1.length);
			expect(info).not.toBeNull();
			expect(info!.rootOffset).toBe(rootOffset);
			expect(info!.isComplete).toBe(false);
		});

		it('resolveArbitraryXisoSplit finds the pair', async () => {
			const { part1, part2 } = truncatedParts();
			const files = { 'p1.bin': part1, 'p2.bin': part2 };
			const result = await resolveArbitraryXisoSplit(
				Object.keys(files),
				nameMap(files),
			);
			expect(result).not.toBeNull();
			expect(result!.parts).toEqual(['p1.bin', 'p2.bin']);
			expect(result!.verify.ok).toBe(true);
		});

		it('resolveBatchEntry reports a Dir split, not a lone standalone File', async () => {
			const { part1, part2 } = truncatedParts();
			const files = { 'p1.bin': part1, 'p2.bin': part2 };
			const resolved = await resolveBatchEntry(Object.keys(files), nameMap(files));
			expect(resolved.kind).toBe('dir');
			if (resolved.kind !== 'dir') throw new Error('unreachable');
			expect(resolved.format).toBe('xiso');
			expect(resolved.parts.map((p) => p.name).sort()).toEqual([
				'p1.bin',
				'p2.bin',
			]);
		});

		it('scanBatch groups the pair into one rawSplit result, not two Unresolved singles', async () => {
			const { part1, part2 } = truncatedParts();
			const results = await scan({ 'p1.bin': part1, 'p2.bin': part2 });
			expect(results).toHaveLength(1);
			expect(results[0].kind).toBe('rawSplit');
			if (results[0].kind !== 'rawSplit') throw new Error('unreachable');
			expect(results[0].parts).toEqual(['p1.bin', 'p2.bin']);
			expect(results[0].verify.ok).toBe(true);
		});
	});

	// Multi-disc packaging semantics
	describe('multi-disc packaging semantics', () => {
		it('reports Unresolved for an independently complete image with no launch executable', async () => {
			const iso = makeFixture({ titleId: 0x41560040 });
			const broken = corruptLaunchExecutableMagic(iso);
			const results = await scan({ 'broken.iso': broken });
			expect(results).toHaveLength(1);
			expect(results[0].kind).toBe('unresolved');
			if (results[0].kind !== 'unresolved') throw new Error('unreachable');
			expect(results[0].names).toEqual(['broken.iso']);
			expect(results[0].reason).toMatch(/no launch executable/i);
		});

		it('groups an incomplete disc set (disc 1 + disc 3, missing disc 2) into one MultiDiscSet', async () => {
			const titleId = 0x41560041;
			const disc1 = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 1, discCount: 3, mediaId: 0x1 },
			);
			const disc3 = patchXexExecutionInfo(
				makeFixture({ titleId, platform: 'x360' }),
				{ discNumber: 3, discCount: 3, mediaId: 0x3 },
			);
			const results = await scan({
				'Game (Disc 1).iso': disc1,
				'Game (Disc 3).iso': disc3,
			});
			expect(results).toHaveLength(1);
			expect(results[0].kind).toBe('multiDiscSet');
			if (results[0].kind !== 'multiDiscSet') throw new Error('unreachable');
			expect(results[0].discCount).toBe(3);
			// Disc 2 simply isn't there - the set just holds the two discs
			// that were actually supplied, no gap-filling and no error.
			expect(results[0].discs).toHaveLength(2);
			expect(
				results[0].discs
					.map((d) => d.discNumber)
					.slice()
					.sort(),
			).toEqual([1, 3]);
		});

		it('does not merge two byte-identical duplicate images - each resolves standalone', async () => {
			const iso = makeFixture({ titleId: 0x41560042 });
			const results = await scan({
				'game.iso': iso,
				'game (copy).iso': iso.slice(),
			});
			expect(results).toHaveLength(2);
			for (const r of results) {
				expect(r.kind).toBe('standalone');
			}
		});
	});

	// Distinct from the "two independently complete discs" guard above -
	// that works because probe_completeness routes whole images into
	// Classification.complete, which raw_split_outcome_for_entry never
	// inspects. Here the third file is unrelated by *format* (STFS magic,
	// filtered out before classify_parts runs), while the other two are
	// a genuine truncated-header + continuation pair. find_raw_split's
	// result isn't checked against entries[0] before being returned, so
	// a caller resolving one file at a time must confirm the match itself.
	describe('resolveBatchEntry does not drop entries[0] in favor of an unrelated split found among its siblings', () => {
		it('resolving a magic-distinct file first must not silently return the xiso split found among its siblings', async () => {
			const stfsMagic = new Uint8Array(64);
			// STFS 'CON ' magic: https://github.com/hetelek/Velocity/blob/master/XboxInternals/Stfs/StfsConstants.h
			stfsMagic.set([0x43, 0x4f, 0x4e, 0x20]);
			const files = {
				'game.stfs-like.bin': stfsMagic,
				'header.bin': isoA.slice(0, cut),
				'continuation.bin': isoA.slice(cut),
			};
			// entries[0] is the unrelated file, resolved before its
			// siblings' split is consumed.
			const resolved = await resolveBatchEntry(
				['game.stfs-like.bin', 'header.bin', 'continuation.bin'],
				nameMap(files),
			);
			if (resolved.kind === 'file') {
				expect(resolved.format).toBe('stfs');
			} else if (resolved.kind === 'dir') {
				expect(resolved.parts.map((p) => p.name)).toContain('game.stfs-like.bin');
			} else {
				throw new Error(
					`expected a resolution describing "game.stfs-like.bin" itself, got ${JSON.stringify(resolved)}`,
				);
			}
		});

		// Same scenario with real file shapes: a genuine `.zar` dropped
		// with an unrelated game's raw-XISO split. Whichever file is
		// resolved first must resolve to *itself*, never silently
		// disappear in favor of a split it isn't part of.
		it('a real zar file resolves standalone when it is entries[0] alongside an unrelated split, matching the reported "3 files in, only 1 entry out" bug', async () => {
			const zarSourceIso = makeFixture({ titleId: 0x5a4a0099 });
			const zarBytes = convertXisoFixtureToBytes(zarSourceIso, {
				format: 'zar',
				outputName: 'other-game',
			});
			const files = {
				'other-game.zar': zarBytes,
				'game.1.iso': isoA.slice(0, cut),
				'game.2.iso': isoA.slice(cut),
			};

			const resolved = await resolveBatchEntry(
				['other-game.zar', 'game.1.iso', 'game.2.iso'],
				nameMap(files),
			);

			expect(resolved.kind).toBe('file');
			if (resolved.kind !== 'file') {
				throw new Error(
					`expected the zar to resolve standalone as its own file, got ${JSON.stringify(resolved)}`,
				);
			}
			expect(resolved.format).toBe('zar');
			expect(resolved.fileSize).toBe(zarBytes.length);
		});

		// Here the genuine header/continuation pair *is* what entries[0]
		// resolves as part of, but an unrelated garbage file elsewhere in
		// the batch (defaults to FileType::Xiso in detect(), landing in
		// the same candidate bucket as the real part 2) gets permuted in
		// by find_raw_split. Every verifiable entry still lives inside
		// the genuine range, so verify_ordering's size check - not the
		// spot-check - is what must reject this ordering.
		it('does not absorb an unrelated garbage file into a genuine split, even when it lands in the same batch', async () => {
			const garbage = new Uint8Array(2048);
			crypto.getRandomValues(garbage);
			const files = {
				'header.bin': isoA.slice(0, cut),
				'continuation.bin': isoA.slice(cut),
				'corrupt.iso': garbage,
			};

			const resolved = await resolveBatchEntry(
				['header.bin', 'continuation.bin', 'corrupt.iso'],
				nameMap(files),
			);

			expect(resolved.kind).toBe('dir');
			if (resolved.kind !== 'dir') throw new Error('unreachable');
			expect(resolved.parts.map((p) => p.name).sort()).toEqual([
				'continuation.bin',
				'header.bin',
			]);
		});

		// End-to-end version of the same scenario: drives resolveBatchEntry
		// one call per remaining file, deleting whatever comes back. The
		// three-file batch must converge to two groups (zar standalone,
		// xiso split paired up), with every file claimed exactly once.
		it('resolves a full three-file zar+split-xiso batch into exactly two entries, with every file accounted for', async () => {
			const zarSourceIso = makeFixture({ titleId: 0x5a4a0098 });
			const zarBytes = convertXisoFixtureToBytes(zarSourceIso, {
				format: 'zar',
				outputName: 'other-game',
			});
			const files = {
				'other-game.zar': zarBytes,
				'game.1.iso': isoA.slice(0, cut),
				'game.2.iso': isoA.slice(cut),
			};

			const remaining = new Set(Object.keys(files));
			const groups: string[][] = [];
			for (const name of Object.keys(files)) {
				if (!remaining.has(name)) continue;
				const siblings = [...remaining].filter((n) => n !== name);
				const resolved = await resolveBatchEntry(
					[name, ...siblings],
					nameMap(files),
				);
				// A 'file' outcome always resolves entries[0] itself (never
				// carries a name of its own) - same assumption
				// groupInputFiles() makes in source.js.
				const claimed =
					resolved.kind === 'file'
						? [name]
						: resolved.kind === 'dir'
							? resolved.parts.map((p) => p.name)
							: resolved.names;
				claimed.forEach((n) => remaining.delete(n));
				groups.push(claimed);
			}

			expect(remaining.size).toBe(0);
			expect(groups).toHaveLength(2);
			const allClaimed = groups.flat().slice().sort();
			expect(allClaimed).toEqual(Object.keys(files).sort());
			const zarGroup = groups.find((g) => g.includes('other-game.zar'));
			expect(zarGroup).toEqual(['other-game.zar']);
			const splitGroup = groups.find((g) => g.includes('game.1.iso'));
			expect(splitGroup?.sort()).toEqual(['game.1.iso', 'game.2.iso']);
		});
	});
});
