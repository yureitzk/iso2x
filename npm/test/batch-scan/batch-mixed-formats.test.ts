import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { makeStfsFixture, STFS_CONTENT_TYPE } from '../utils/fixtures/stfs.js';
import { makeAccountFileBytes } from '../utils/fixtures/account.js';
import { resolveBatchEntry, scanBatch } from '../../dist/index.js';
import { checkIsoCompleteness } from '../../dist/detect-advanced.js';
import type { SourceReadFn } from '../../dist/index.js';
import { makeReadFn, nameMap, scan, only, namesOf } from '../utils/read-fns.js';
import {
	convertXisoFixtureToBytes,
	convertXisoFixtureToExtractedParts,
	convertXisoFixtureToGodParts,
	parseCciLayout,
	splitCciAt,
} from '../utils/session-helpers.js';

beforeAll(async () => {
	await setupWasm();
});

/** Drives resolveBatchEntry in a loop until every file has been claimed,
 * the way a caller resolving files one at a time would. */
async function groupAll(
	files: Record<string, Uint8Array>,
): Promise<string[][]> {
	const accessor = nameMap(files);
	const remaining = new Set(Object.keys(files));
	const groups: string[][] = [];
	for (const name of Object.keys(files)) {
		if (!remaining.has(name)) continue;
		const siblings = [...remaining].filter((n) => n !== name);
		const resolved = await resolveBatchEntry([name, ...siblings], accessor);
		const claimed =
			resolved.kind === 'file'
				? [name]
				: resolved.kind === 'dir'
					? resolved.parts.map((p) => p.name)
					: resolved.names;
		claimed.forEach((n) => remaining.delete(n));
		groups.push(claimed);
	}
	return groups;
}

// STFS packages are content-addressed by SHA1, so the same package can
// appear under multiple paths with an identical filename - not a collision.
const CONTENT_PATHS = [
	'Content/0000000000000000/504787D8/000D0000/281104A67F7C961E2736A5F6F524D43FC81EB79158',
	'Content/0000000000000000/584107EF/000D0000/4EBF4EDF58B9F1E642FD604B56844DBC2615A20A58',
	'Content/0000000000000000/58410889/000D0000/87D0A5D366F24C8FF8BEE120E5F72D78F100E18958',
	'Content/0000000000000000/584109FF/000D0000/281104A67F7C961E2736A5F6F524D43FC81EB79158',
] as const;

const NON_MAGIC_ASSET_PATHS = [
	'AvatarAssetPack/nxeart',
	'en/Music.xwb',
	'fr/Sounds.xwb',
	'jp/XBLAVolume3.xgs',
	'pvz_button.png',
] as const;

describe('mixed-format batch', () => {
	let xisoHeader: Uint8Array;
	let xisoContinuation: Uint8Array;
	let standaloneXiso: Uint8Array;
	let cciPart1: Uint8Array;
	let cciPart2: Uint8Array;
	let stfsBytes: Uint8Array;
	let godDataPart: Uint8Array;
	let garbage: Uint8Array;
	let extractedParts: { name: string; size: number; readFn: SourceReadFn }[];

	beforeAll(() => {
		const splitSource = makeFixture({ titleId: 0x4d495801 });
		const info = checkIsoCompleteness(
			makeReadFn(splitSource),
			splitSource.length,
		);
		expect(info).toBeDefined();
		expect(info!.isComplete).toBe(true);
		const cut = info!.rootOffset + info!.maxUsedPrefixSize - 0x400;
		xisoHeader = splitSource.slice(0, cut);
		xisoContinuation = splitSource.slice(cut);

		standaloneXiso = makeFixture({ titleId: 0x4d495802 });

		const cciSource = makeFixture({ titleId: 0x4d495803 });
		const cciBytes = convertXisoFixtureToBytes(cciSource, {
			format: 'cci',
			outputName: 'game',
		});
		const layout = parseCciLayout(cciBytes);
		const splitSector = Math.max(1, Math.floor(layout.totalSectors / 2));
		({ part1: cciPart1, part2: cciPart2 } = splitCciAt(cciBytes, splitSector));

		stfsBytes = makeStfsFixture({ fileName: 'default.xex' }).bytes;

		const godSource = makeFixture({ titleId: 0x4d495804, platform: 'x360' });
		const { dataParts } = convertXisoFixtureToGodParts(godSource);
		expect(dataParts).toHaveLength(1);
		godDataPart = dataParts[0]!.readFn(0, dataParts[0]!.size);

		garbage = new Uint8Array(2048);
		crypto.getRandomValues(garbage);

		const extractedSource = makeFixture({
			titleId: 0x4d495808,
			platform: 'x360',
			includeSystemUpdate: true,
		});
		extractedParts = convertXisoFixtureToExtractedParts(extractedSource);
	});

	it('classifies five unrelated formats dropped together, each result naming only its own files', async () => {
		const files = {
			'solo.iso': standaloneXiso,
			'game.1.cci': cciPart1,
			'game.2.cci': cciPart2,
			'profile.stfs': stfsBytes,
			'Title.data/Data0000': godDataPart,
			'noise.bin': garbage,
		};
		const results = await scan(files);

		expect(results).toHaveLength(6);

		const standalone = only(results, 'standalone');
		expect(standalone.names).toEqual(['solo.iso']);

		const godFolder = only(results, 'godFolder');
		expect(godFolder.names).toEqual(['Title.data/Data0000']);

		const unresolved = results.filter((r) => r.kind === 'unresolved');
		expect(unresolved).toHaveLength(4);
		const unresolvedNames = unresolved.flatMap((r) => namesOf(r)).sort();
		expect(unresolvedNames).toEqual(
			['game.1.cci', 'game.2.cci', 'profile.stfs', 'noise.bin'].sort(),
		);

		const allNames = results.flatMap(namesOf).sort();
		expect(allNames).toEqual(Object.keys(files).sort());
	});

	it('groups a genuine raw-XISO split into one rawSplit result alongside unrelated formats', async () => {
		const files = {
			'split.1.iso': xisoHeader,
			'split.2.iso': xisoContinuation,
			'solo.iso': standaloneXiso,
			'profile.stfs': stfsBytes,
			'Title.data/Data0000': godDataPart,
		};
		const results = await scan(files);

		expect(results).toHaveLength(4);

		const rawSplit = only(results, 'rawSplit');
		expect(rawSplit.parts.slice().sort()).toEqual(
			['split.1.iso', 'split.2.iso'].sort(),
		);

		const standalone = only(results, 'standalone');
		expect(standalone.names).toEqual(['solo.iso']);

		const godFolder = only(results, 'godFolder');
		expect(godFolder.names).toEqual(['Title.data/Data0000']);

		const stfsUnresolved = only(results, 'unresolved');
		expect(stfsUnresolved.names).toEqual(['profile.stfs']);
	});

	it('resolveBatchEntry converges a mixed batch into the right groups, every file claimed exactly once', async () => {
		const files = {
			'split.1.iso': xisoHeader,
			'split.2.iso': xisoContinuation,
			'solo.iso': standaloneXiso,
			'profile.stfs': stfsBytes,
			'noise.bin': garbage,
		};
		const groups = await groupAll(files);

		expect(groups).toHaveLength(4);
		const allClaimed = groups.flat().slice().sort();
		expect(allClaimed).toEqual(Object.keys(files).sort());

		const splitGroup = groups.find((g) => g.includes('split.1.iso'));
		expect(splitGroup?.sort()).toEqual(['split.1.iso', 'split.2.iso']);
		expect(groups).toContainEqual(['solo.iso']);
		expect(groups).toContainEqual(['profile.stfs']);
		expect(groups).toContainEqual(['noise.bin']);
	});

	it('resolveBatchEntry does not let an unrelated CCI split hijack the resolution of a different entry', async () => {
		const splitSource = makeFixture({ titleId: 0x4d495805 });
		const cciBytes = convertXisoFixtureToBytes(splitSource, {
			format: 'cci',
			outputName: 'other-game',
		});
		const layout = parseCciLayout(cciBytes);
		const splitSector = Math.max(1, Math.floor(layout.totalSectors / 2));
		const { part1, part2 } = splitCciAt(cciBytes, splitSector);
		const stfs = makeStfsFixture({ fileName: 'default.xex' }).bytes;
		const files = {
			'profile.stfs': stfs,
			'other-game.1.cci': part1,
			'other-game.2.cci': part2,
		};
		const resolved = await resolveBatchEntry(
			['profile.stfs', 'other-game.1.cci', 'other-game.2.cci'],
			nameMap(files),
		);

		expect(resolved.kind).toBe('file');
		if (resolved.kind !== 'file') throw new Error('unreachable');
		expect(resolved.format).toBe('stfs');
	});

	it('converges a genuine CCI split plus an unrelated STFS file into exactly two groups', async () => {
		const splitSource = makeFixture({ titleId: 0x4d495806 });
		const cciBytes = convertXisoFixtureToBytes(splitSource, {
			format: 'cci',
			outputName: 'game',
		});
		const layout = parseCciLayout(cciBytes);
		const splitSector = Math.max(1, Math.floor(layout.totalSectors / 2));
		const { part1, part2 } = splitCciAt(cciBytes, splitSector);
		const stfs = makeStfsFixture({ fileName: 'default.xex' }).bytes;
		const files = {
			'profile.stfs': stfs,
			'game.1.cci': part1,
			'game.2.cci': part2,
		};
		const groups = await groupAll(files);

		expect(groups).toHaveLength(2);
		const stfsGroup = groups.find((g) => g.includes('profile.stfs'));
		expect(stfsGroup).toEqual(['profile.stfs']);
		const cciGroup = groups.find((g) => g.includes('game.1.cci'));
		expect(cciGroup?.sort()).toEqual(['game.1.cci', 'game.2.cci']);
	});

	it('scanBatch resolves a real split correctly and still reports an unrelated bystander file sharing its continuation shape', async () => {
		const splitSource = makeFixture({ titleId: 0x4d495807 });
		const info = checkIsoCompleteness(
			makeReadFn(splitSource),
			splitSource.length,
		);
		expect(info).toBeDefined();
		expect(info!.isComplete).toBe(true);
		const cut = info!.rootOffset + info!.maxUsedPrefixSize - 0x400;
		const header = splitSource.slice(0, cut);
		const continuation = splitSource.slice(cut);

		const bystander = new Uint8Array(continuation.length);
		crypto.getRandomValues(bystander);

		const files = {
			'header.bin': header,
			'continuation.bin': continuation,
			'bystander.bin': bystander,
		};
		const results = await scan(files);

		const rawSplit = only(results, 'rawSplit');
		expect(rawSplit.parts.slice().sort()).toEqual(
			['header.bin', 'continuation.bin'].sort(),
		);

		const allNames = results.flatMap(namesOf);
		expect(allNames).toContain('bystander.bin');
	});

	it('resolves every nested STFS package individually and groups the headerless bystanders into one unresolved report, never as a raw-XISO grouping', async () => {
		const profilePackage = makeStfsFixture({
			magic: 'CON ',
			contentType: STFS_CONTENT_TYPE.arcadeGame, // 0xD0000, matches "000D0000"
			fileName: 'Account',
			fileBytes: makeAccountFileBytes({ gamertag: 'Test Player' }, false),
		}).bytes;
		const downloadedPackage = makeStfsFixture({
			magic: 'PIRS',
			contentType: STFS_CONTENT_TYPE.arcadeGame,
			fileName: 'default.xex',
		}).bytes;

		const defaultXex = extractedParts.find((p) => p.name === 'default.xex')!;

		const files: Record<string, Uint8Array> = {
			'default.xex': defaultXex.readFn(0, defaultXex.size),
			[CONTENT_PATHS[0]]: profilePackage,
			[CONTENT_PATHS[1]]: profilePackage, // same package, second owning path
			[CONTENT_PATHS[2]]: downloadedPackage,
			[CONTENT_PATHS[3]]: downloadedPackage, // same package, second owning path
		};
		for (const path of NON_MAGIC_ASSET_PATHS) {
			const bytes = new Uint8Array(2048);
			crypto.getRandomValues(bytes);
			files[path] = bytes;
		}

		const results = await scan(files);

		expect(
			results.every((r) => r.kind === 'unresolved'),
			`expected every result to be unresolved, got ${JSON.stringify(results)}`,
		).toBe(true);

		const allNames = results.flatMap(namesOf).sort();
		expect(allNames).toEqual(Object.keys(files).sort());

		// The 4 STFS packages each resolve as their own single-name result.
		const stfsPackageResults = results.filter((r) =>
			CONTENT_PATHS.includes(namesOf(r)[0] as (typeof CONTENT_PATHS)[number]),
		);
		expect(stfsPackageResults).toHaveLength(4);
		for (const r of stfsPackageResults) {
			expect(namesOf(r)).toHaveLength(1);
		}

		// default.xex + every non-magic asset land in one combined result.
		const headerlessBystanders = results.find(
			(r) => !stfsPackageResults.includes(r),
		)!;
		expect(namesOf(headerlessBystanders).sort()).toEqual(
			['default.xex', ...NON_MAGIC_ASSET_PATHS].sort(),
		);
		expect(results).toHaveLength(5);
	});

	it('detects a genuine gamer-profile package (CON-signed, real Account file, Profile content type, FFFE07D1/00010000 path) correctly during a mixed bulk scan', async () => {
		// Mirrors a real console layout: Content/<profileId>/FFFE07D1
		// (the Dashboard's pseudo-title-ID)/00010000 (Profile content
		// type)/<profileId again, as the STFS package's filename>.
		const profileId = 'E00005538DC276AE';
		const profilePath = `Content/${profileId}/FFFE07D1/00010000/${profileId}`;
		const profilePackage = makeStfsFixture({
			magic: 'CON ',
			contentType: STFS_CONTENT_TYPE.profile,
			fileName: 'Account',
			fileBytes: makeAccountFileBytes({ gamertag: 'Test Player' }, false),
		}).bytes;

		const splitSource = makeFixture({ titleId: 0x4d495810 });
		const info = checkIsoCompleteness(
			makeReadFn(splitSource),
			splitSource.length,
		);
		expect(info).toBeDefined();
		expect(info!.isComplete).toBe(true);
		const cut = info!.rootOffset + info!.maxUsedPrefixSize - 0x400;
		const header = splitSource.slice(0, cut);
		const continuation = splitSource.slice(cut);

		const files = {
			[profilePath]: profilePackage,
			'split.1.iso': header,
			'split.2.iso': continuation,
			'solo.iso': standaloneXiso,
			'Title.data/Data0000': godDataPart,
			'noise.bin': garbage,
		};
		const results = await scan(files);

		expect(results).toHaveLength(5);

		const rawSplit = only(results, 'rawSplit');
		expect(rawSplit.parts.slice().sort()).toEqual(
			['split.1.iso', 'split.2.iso'].sort(),
		);

		const standalone = only(results, 'standalone');
		expect(standalone.names).toEqual(['solo.iso']);

		const godFolder = only(results, 'godFolder');
		expect(godFolder.names).toEqual(['Title.data/Data0000']);

		const profileResult = results.find((r) => namesOf(r).includes(profilePath))!;
		expect(profileResult.kind).toBe('unresolved');
		expect(namesOf(profileResult)).toEqual([profilePath]);

		// Confirms resolveBatchEntry independently identifies it as an
		// STFS file (not, say, a raw-XISO fragment) when probed directly.
		const resolved = await resolveBatchEntry(
			[profilePath, 'solo.iso'],
			nameMap(files),
		);
		expect(resolved.kind).toBe('file');
		if (resolved.kind !== 'file') throw new Error('unreachable');
		expect(resolved.format).toBe('stfs');
	});

	it('resolves a genuine raw-XISO split correctly with a full Content/ dump sitting alongside it as noise', async () => {
		// An extracted-title + Content/ export sitting alongside an
		// unrelated, genuinely split disc image, exercising
		// find_raw_split against a realistically sized bystander set.
		const splitSource = makeFixture({ titleId: 0x4d495809, platform: 'x360' });
		const info = checkIsoCompleteness(
			makeReadFn(splitSource),
			splitSource.length,
		);
		expect(info).toBeDefined();
		expect(info!.isComplete).toBe(true);
		const cut = info!.rootOffset + info!.maxUsedPrefixSize - 0x400;
		const header = splitSource.slice(0, cut);
		const continuation = splitSource.slice(cut);

		const profilePackage = makeStfsFixture({
			magic: 'CON ',
			contentType: STFS_CONTENT_TYPE.arcadeGame,
			fileName: 'Account',
			fileBytes: makeAccountFileBytes({ gamertag: 'Test Player 2' }, false),
		}).bytes;

		const defaultXex = extractedParts.find((p) => p.name === 'default.xex')!;

		const files: Record<string, Uint8Array> = {
			'split.1.iso': header,
			'split.2.iso': continuation,
			'default.xex': defaultXex.readFn(0, defaultXex.size),
			[CONTENT_PATHS[0]]: profilePackage,
			[CONTENT_PATHS[1]]: profilePackage,
		};
		for (const path of NON_MAGIC_ASSET_PATHS) {
			const bytes = new Uint8Array(2048);
			crypto.getRandomValues(bytes);
			files[path] = bytes;
		}

		const start = Date.now();
		const results = await scan(files);
		const elapsedMs = Date.now() - start;

		// Bounds the combinatorial search this scenario stresses.
		expect(elapsedMs).toBeLessThan(5000);

		const rawSplit = only(results, 'rawSplit');
		expect(rawSplit.parts.slice().sort()).toEqual(
			['split.1.iso', 'split.2.iso'].sort(),
		);

		const unresolved = results.filter((r) => r.kind === 'unresolved');
		const unresolvedNames = unresolved.flatMap((r) => namesOf(r)).sort();
		expect(unresolvedNames).toEqual(
			[
				'default.xex',
				...CONTENT_PATHS.slice(0, 2),
				...NON_MAGIC_ASSET_PATHS,
			].sort(),
		);
	});

	it('a duplicate-named continuation entry does not turn a clean, resolvable split into a false "ambiguous"', async () => {
		const splitSource = makeFixture({ titleId: 0x4d49580a });
		const info = checkIsoCompleteness(
			makeReadFn(splitSource),
			splitSource.length,
		);
		expect(info).toBeDefined();
		expect(info!.isComplete).toBe(true);
		const cut = info!.rootOffset + info!.maxUsedPrefixSize - 0x400;
		const header = splitSource.slice(0, cut);
		const continuation = splitSource.slice(cut);

		const files: Record<string, Uint8Array> = {
			'header.bin': header,
			'continuation.bin': continuation,
		};
		// 'continuation.bin' listed twice - same name, same bytes.
		const entries = ['header.bin', 'continuation.bin', 'continuation.bin'];
		const results = await scanBatch(entries, nameMap(files));

		// Every input entry is still accounted for somewhere.
		const totalNamesReported = results.flatMap(namesOf).length;
		expect(totalNamesReported).toBe(entries.length);

		const rawSplits = results.filter((r) => r.kind === 'rawSplit');
		expect(
			rawSplits,
			`expected the genuine split to still resolve despite the duplicate entry, got ${JSON.stringify(results)}`,
		).toHaveLength(1);
	});

	it('two entries whose names are identical except for case are still both accounted for and do not break a clean, resolvable split', async () => {
		const splitSource = makeFixture({ titleId: 0x4d49580b });
		const info = checkIsoCompleteness(
			makeReadFn(splitSource),
			splitSource.length,
		);
		expect(info).toBeDefined();
		expect(info!.isComplete).toBe(true);
		const cut = info!.rootOffset + info!.maxUsedPrefixSize - 0x400;
		const header = splitSource.slice(0, cut);
		const continuation = splitSource.slice(cut);

		const files: Record<string, Uint8Array> = {
			'header.bin': header,
			'continuation.bin': continuation,
			'CONTINUATION.BIN': continuation,
		};
		const entries = ['header.bin', 'continuation.bin', 'CONTINUATION.BIN'];
		const results = await scanBatch(entries, nameMap(files));

		// Every input entry is still accounted for somewhere.
		const totalNamesReported = results.flatMap(namesOf).length;
		expect(totalNamesReported).toBe(entries.length);

		const rawSplits = results.filter((r) => r.kind === 'rawSplit');
		expect(
			rawSplits,
			`expected the genuine split to still resolve despite the case-differing duplicate, got ${JSON.stringify(results)}`,
		).toHaveLength(1);
	});

	it('two unrelated standalone files whose names differ only by case both resolve independently, never merged or dropped', async () => {
		const titleA = 0x4d49580c;
		const titleB = 0x4d49580d;
		const isoA = makeFixture({ titleId: titleA });
		const isoB = makeFixture({ titleId: titleB });
		const files: Record<string, Uint8Array> = {
			'game.iso': isoA,
			'GAME.ISO': isoB,
		};
		const results = await scan(files);

		expect(results).toHaveLength(2);
		const standalones = results.filter((r) => r.kind === 'standalone');
		expect(standalones).toHaveLength(2);
		const titleIds = standalones
			.map((r) => (r.kind === 'standalone' ? r.titleId : undefined))
			.slice()
			.sort();
		expect(titleIds).toEqual(
			[titleA, titleB]
				.map((t) => t.toString(16).toUpperCase())
				.slice()
				.sort(),
		);

		const allNames = results.flatMap(namesOf).sort();
		expect(allNames).toEqual(Object.keys(files).sort());
	});

	it('two independent, genuine XISO splits whose header/continuation names collide with each other only by case resolve as two separate results, never conflated', async () => {
		const titleA = 0x4d49580e;
		const splitSourceA = makeFixture({ titleId: titleA });
		const infoA = checkIsoCompleteness(
			makeReadFn(splitSourceA),
			splitSourceA.length,
		);
		expect(infoA).toBeDefined();
		expect(infoA!.isComplete).toBe(true);
		const cutA = infoA!.rootOffset + infoA!.maxUsedPrefixSize - 0x400;
		const headerA = splitSourceA.slice(0, cutA);
		const continuationA = splitSourceA.slice(cutA);

		const titleB = 0x4d49580f;
		// includeSystemUpdate gives this fixture a different volume size
		// (and continuation length) than splitSourceA - otherwise both
		// fixtures' trailing 0x400-byte continuations would be identical
		// zero padding, indistinguishable from either header's perspective.
		// Real splits from different titles essentially never need the
		// same trailing byte count, so this mirrors what disambiguates
		// them in practice.
		const splitSourceB = makeFixture({
			titleId: titleB,
			includeSystemUpdate: true,
		});
		const infoB = checkIsoCompleteness(
			makeReadFn(splitSourceB),
			splitSourceB.length,
		);
		expect(infoB).toBeDefined();
		expect(infoB!.isComplete).toBe(true);
		const cutB = infoB!.rootOffset + infoB!.maxUsedPrefixSize - 0x400;
		const headerB = splitSourceB.slice(0, cutB);
		const continuationB = splitSourceB.slice(cutB);

		const files: Record<string, Uint8Array> = {
			'split.header.bin': headerA,
			'split.continuation.bin': continuationA,
			'SPLIT.HEADER.BIN': headerB,
			'SPLIT.CONTINUATION.BIN': continuationB,
		};
		const results = await scan(files);

		const rawSplits = results.filter((r) => r.kind === 'rawSplit');
		expect(
			rawSplits,
			`expected two independent splits to resolve separately, got ${JSON.stringify(results)}`,
		).toHaveLength(2);
		if (rawSplits.some((r) => r.kind !== 'rawSplit')) {
			throw new Error('unreachable');
		}

		const groupedNames = rawSplits
			.map((r) => (r.kind === 'rawSplit' ? r.parts.slice().sort() : []))
			.sort();
		expect(groupedNames).toEqual(
			[
				['split.continuation.bin', 'split.header.bin'],
				['SPLIT.CONTINUATION.BIN', 'SPLIT.HEADER.BIN'],
			].sort(),
		);

		const allNames = results.flatMap(namesOf).sort();
		expect(allNames).toEqual(Object.keys(files).sort());
	});
});
