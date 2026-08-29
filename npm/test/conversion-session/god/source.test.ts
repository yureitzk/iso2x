import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import {
	makeFixture,
	DEFAULT_XBE_DECLARED_SIZE,
} from '../../utils/fixtures/xsf.js';
import {
	ConversionSession,
	inspectSource,
	cisoSectorSize,
	cciSectorSize,
} from '../../../dist/index.js';
import {
	makeReadFn,
	nullReadFn,
	throwingReadFn,
} from '../../utils/read-fns.js';
import {
	convertXisoFixtureToGodParts,
	driveHashing,
	drain,
	concat,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import { GOD_SOURCE_OPTIONS } from '../../utils/sources.js';

beforeAll(setupWasm);

let CISO_SECTOR_SIZE: number;
let CCI_SECTOR_SIZE: number;

beforeAll(() => {
	CISO_SECTOR_SIZE = cisoSectorSize();
	CCI_SECTOR_SIZE = cciSectorSize();
});

describe('ConversionSession(god source) error paths', () => {
	const iso = makeFixture({ titleId: 0x6a6a0010 });
	let godParts: ReturnType<typeof convertXisoFixtureToGodParts>['dataParts'];

	beforeAll(() => {
		({ dataParts: godParts } = convertXisoFixtureToGodParts(iso, {
			format: 'god',
		}));
	});

	it('throws for a zeroed (invalid) set of parts', () => {
		const zeroedParts = godParts.map((part) => ({
			...part,
			readFn: (_offset: number, length: number) => new Uint8Array(length),
		}));
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'extracted' },
				{ source: GOD_SOURCE_OPTIONS, parts: zeroedParts },
			),
		).toThrow();
	});

	it('propagates errors thrown inside a part readFn', () => {
		const throwingParts = godParts.map((part) => ({
			...part,
			readFn: throwingReadFn,
		}));
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'extracted' },
				{ source: GOD_SOURCE_OPTIONS, parts: throwingParts },
			),
		).toThrow('read error from JS');
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'extracted' },
				{ source: GOD_SOURCE_OPTIONS, parts: [] },
			),
		).toThrow();
	});

	it('throws when sourceParts is omitted for a god source, instead of falling back to a single-part read', () => {
		// Unlike xiso (where a single implicit part built from readFn/fileSize
		// is a valid fallback - see parts_from_js), a god source requires
		// explicit sourceParts: there's no world where reading a lone blob
		// starting at file offset 0 recovers Data%04d part semantics.
		expect(() =>
			ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'extracted' },
				{ source: GOD_SOURCE_OPTIONS },
			),
		).toThrow();
	});
});

describe('ConversionSession(god source) => extracted', () => {
	// Same fixture/offsets ConversionSession(extracted) in extracted.test.ts
	// asserts against directly off the raw xiso bytes - reused here so the
	// only variable between that test and this one is going through god
	// parts as the source instead of the raw image.
	const iso = makeFixture({ titleId: 0x6a6a0011 });
	let godParts: ReturnType<typeof convertXisoFixtureToGodParts>['dataParts'];

	beforeAll(() => {
		({ dataParts: godParts } = convertXisoFixtureToGodParts(iso, {
			format: 'god',
		}));
	});

	it('opens without throwing', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'extracted' },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		session.free();
	});

	it('totalUnits equals the file count (1 for the fixture), same as reading straight off the xiso', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'extracted' },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		expect(session.totalUnits()).toBe(1);
		session.free();
	});

	it('outputManifest matches what extracting the same fixture straight from xiso produces', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'extracted' },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		const manifest = session.outputManifest();
		session.free();
		expect(manifest).toEqual([
			{ name: 'default.xbe', size: DEFAULT_XBE_DECLARED_SIZE },
		]);
	});

	it('extracted bytes match the original xiso bytes at the recorded offset, round-tripped through god parts', () => {
		// The fixture's default.xbe starts at sector 0x22 and the directory
		// entry declares its size as DEFAULT_XBE_DECLARED_SIZE (see
		// xsf.ts) - identical expectation to the direct-from-xiso
		// case, proving GodSource's remap_sector reconstructs the exact same
		// XDVDFS payload bytes.
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'extracted' },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		session.free();
		const expectedOffset = 0x22 * 0x800;
		const expectedSize = DEFAULT_XBE_DECLARED_SIZE;
		expect(chunk.length).toBe(expectedSize);
		expect(chunk).toEqual(
			iso.slice(expectedOffset, expectedOffset + expectedSize),
		);
	});

	it('round-trips for a god fixture built with mode: "none" (Direct backend)', () => {
		// Mirrors the inspectSource coverage in inspect-source/god.test.ts for
		// the Direct backend's mode: 'none'/'partial' - this confirms
		// ConversionSession, not just inspection, reads through it correctly.
		const noneIso = makeFixture({ titleId: 0x6a6a0012 });
		const { dataParts: parts } = convertXisoFixtureToGodParts(noneIso, {
			format: 'god',
			mode: 'none',
		});
		const session = ConversionSession.open(
			nullReadFn,
			noneIso.length,
			{ format: 'extracted' },
			{ source: GOD_SOURCE_OPTIONS, parts },
		);
		const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		session.free();
		const expectedOffset = 0x22 * 0x800;
		const expectedSize = DEFAULT_XBE_DECLARED_SIZE;
		expect(chunk).toEqual(
			noneIso.slice(expectedOffset, expectedOffset + expectedSize),
		);
	});

	it('round-trips for a god fixture built with mode: "partial" (Direct backend, trim + zero)', () => {
		const partialIso = makeFixture({ titleId: 0x6a6a0013 });
		const { dataParts: parts } = convertXisoFixtureToGodParts(partialIso, {
			format: 'god',
			mode: 'partial',
		});
		const session = ConversionSession.open(
			nullReadFn,
			partialIso.length,
			{ format: 'extracted' },
			{ source: GOD_SOURCE_OPTIONS, parts },
		);
		const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		session.free();
		const expectedOffset = 0x22 * 0x800;
		const expectedSize = DEFAULT_XBE_DECLARED_SIZE;
		expect(chunk).toEqual(
			partialIso.slice(expectedOffset, expectedOffset + expectedSize),
		);
	});

	// Open item #7 in the doc flagged that ciso/source.test.ts's => extracted
	// block is missing this test, without confirming whether the omission is
	// deliberate. Adding it here rather than silently carrying the possible
	// gap forward - if ciso's omission turns out to be intentional (manifest
	// name-matching already covering determinism indirectly), this can be
	// dropped to match, but the safer default is coverage, not a silent copy.
	it('is deterministic across separate sessions', () => {
		const a = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'extracted' },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		const chunkA = a.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		a.free();
		const b = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'extracted' },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		const chunkB = b.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		b.free();
		expect(chunkA).toEqual(chunkB);
	});
});

describe('ConversionSession(god source) => xiso', () => {
	const iso = makeFixture({ titleId: 0x6a6a0014 });
	let godParts: ReturnType<typeof convertXisoFixtureToGodParts>['dataParts'];

	beforeAll(() => {
		({ dataParts: godParts } = convertXisoFixtureToGodParts(iso, {
			format: 'god',
		}));
	});

	it('opens and drains without throwing', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'xiso' },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	it('rebuilt xiso reports the same titleId/contentType as the original fixture', () => {
		// xiso's default target mode is a full XDVDFS reauthor (same family as
		// GodBackend::Rebuild), so this isn't asserting byte-identical output -
		// it's asserting the volume that comes back out the other end still
		// carries the same identity, i.e. the god source really did hand the
		// writer a working XDVDFS tree and not silently-wrong bytes.
		const original = inspectSource(makeReadFn(iso), iso.length, {
			source: { format: 'xiso' },
		});
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'xiso' },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		const chunks: Uint8Array[] = [];
		while (!session.isDone()) {
			const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE);
			if (chunk) chunks.push(chunk);
		}
		session.free();
		const rebuilt = concat(chunks);
		const rebuiltInfo = inspectSource(makeReadFn(rebuilt), rebuilt.length, {
			source: { format: 'xiso' },
		});
		expect(rebuiltInfo.titleId).toBe(original.titleId);
		expect(rebuiltInfo.contentType).toBe(original.contentType);
	});
});

describe('ConversionSession(god source) => ciso', () => {
	const iso = makeFixture({ titleId: 0x6a6a0020 });
	let godParts: ReturnType<typeof convertXisoFixtureToGodParts>['dataParts'];

	beforeAll(() => {
		({ dataParts: godParts } = convertXisoFixtureToGodParts(iso, {
			format: 'god',
		}));
	});

	const CISO_OUTPUT_NAME = 'game';
	it('opens and drains without throwing', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'ciso', outputName: CISO_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		driveHashing(session);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	// Same "identity survives the round trip" shape as the => xiso block
	// above, just pointed at ciso's own inspector instead of xiso's.
	it('rebuilt ciso reports the same titleId/contentType as the original fixture', () => {
		const original = inspectSource(makeReadFn(iso), iso.length, {
			source: { format: 'xiso' },
		});
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'ciso', outputName: CISO_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		driveHashing(session);
		const out = drain(session, UNBOUNDED_CHUNK_SIZE);
		const rebuiltInfo = inspectSource(makeReadFn(out), out.length, {
			source: { format: 'ciso' },
		});
		expect(rebuiltInfo.titleId).toBe(original.titleId);
		expect(rebuiltInfo.contentType).toBe(original.contentType);
	});

	// Mirrors ciso/errors-and-basics.test.ts's "currentEntryName is
	// <outputName>.1.cso" contract - just confirming the naming holds when
	// the source is god instead of xiso, since the target-side code that
	// names split files doesn't know or care what kind of source fed it.
	it('outputManifest has a single "game.cso" entry, under the split threshold', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'ciso', outputName: CISO_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		const out = drain(session, UNBOUNDED_CHUNK_SIZE);
		expect(manifest).toHaveLength(1);
		expect(manifest[0].name).toBe(`${CISO_OUTPUT_NAME}.cso`);
		expect(manifest[0].size).toBe(out.length);
	});

	it('CISO header uncompressed_size equals totalUnits * sector size, same contract as the xiso-sourced case', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'ciso', outputName: CISO_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		driveHashing(session);
		const totalUnits = session.totalUnits();
		const header = session.nextChunk(32)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getBigUint64(8, true)).toBe(
			BigInt(totalUnits * CISO_SECTOR_SIZE),
		);
	});

	it('totalUnits (sector count) matches converting the same fixture straight from xiso', () => {
		const fromGod = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'ciso', outputName: CISO_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		driveHashing(fromGod);
		const godUnits = fromGod.totalUnits();
		fromGod.free();
		const fromXiso = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'ciso', outputName: CISO_OUTPUT_NAME },
			{ source: { format: 'xiso' } },
		);
		driveHashing(fromXiso);
		const xisoUnits = fromXiso.totalUnits();
		fromXiso.free();
		expect(godUnits).toBeGreaterThan(0);
		expect(godUnits).toBe(xisoUnits);
	});
});

describe('ConversionSession(god source) => cci', () => {
	const iso = makeFixture({ titleId: 0x6a6a0021 });
	let godParts: ReturnType<typeof convertXisoFixtureToGodParts>['dataParts'];

	beforeAll(() => {
		({ dataParts: godParts } = convertXisoFixtureToGodParts(iso, {
			format: 'god',
		}));
	});

	const CCI_OUTPUT_NAME = 'game';
	it('opens and drains without throwing', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'cci', outputName: CCI_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		driveHashing(session);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	it('rebuilt cci reports the same titleId/contentType as the original fixture', () => {
		const original = inspectSource(makeReadFn(iso), iso.length, {
			source: { format: 'xiso' },
		});
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'cci', outputName: CCI_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		driveHashing(session);
		const out = drain(session, UNBOUNDED_CHUNK_SIZE);
		const rebuiltInfo = inspectSource(makeReadFn(out), out.length, {
			source: { format: 'cci' },
		});
		expect(rebuiltInfo.titleId).toBe(original.titleId);
		expect(rebuiltInfo.contentType).toBe(original.contentType);
	});

	it('outputManifest has a single "game.cci" entry, under the split threshold', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'cci', outputName: CCI_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		const out = drain(session, UNBOUNDED_CHUNK_SIZE);
		expect(manifest).toHaveLength(1);
		expect(manifest[0].name).toBe(`${CCI_OUTPUT_NAME}.cci`);
		expect(manifest[0].size).toBe(out.length);
	});

	// Cross-checks the CCI header's uncompressed_size field (see cci.rs's
	// header layout, exercised directly off xiso in
	// cci/output-format.test.ts) still reflects the repacked image size when
	// the bytes underneath came from a god image instead of a raw xiso.
	it('CCI header uncompressed_size equals totalUnits * sector size, same contract as the xiso-sourced case', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'cci', outputName: CCI_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		driveHashing(session);
		const totalUnits = session.totalUnits();
		const header = session.nextChunk(32)!;
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getBigUint64(8, true)).toBe(BigInt(totalUnits * CCI_SECTOR_SIZE));
	});
});

describe('ConversionSession(god source) => zar', () => {
	const iso = makeFixture({ titleId: 0x6a6a0022 });
	let godParts: ReturnType<typeof convertXisoFixtureToGodParts>['dataParts'];

	beforeAll(() => {
		({ dataParts: godParts } = convertXisoFixtureToGodParts(iso, {
			format: 'god',
		}));
	});

	const ZAR_OUTPUT_NAME = 'game';

	it('opens and drains without throwing', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'zar', outputName: ZAR_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	// zar's hashNextPart() is a no-op for every source (see
	// zar/errors-and-basics.test.ts's "same as xiso/extracted" case) - worth
	// re-asserting here since god is the one source format that genuinely
	// does need a hashing pass when *it's* the target.
	it('hashNextPart is a no-op that returns true immediately', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'zar', outputName: ZAR_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		expect(session.hashNextPart()).toBe(true);
		session.free();
	});

	it('rebuilt zar reports the same titleId/contentType as the original fixture', () => {
		const original = inspectSource(makeReadFn(iso), iso.length, {
			source: { format: 'xiso' },
		});
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'zar', outputName: ZAR_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		const out = drain(session, UNBOUNDED_CHUNK_SIZE);
		const rebuiltInfo = inspectSource(makeReadFn(out), out.length, {
			source: { format: 'zar' },
		});
		expect(rebuiltInfo.titleId).toBe(original.titleId);
		expect(rebuiltInfo.contentType).toBe(original.contentType);
	});

	it("totalUnits equals the fixture's declared default.xbe size, same as converting straight from xiso", () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'zar', outputName: ZAR_OUTPUT_NAME },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		expect(session.totalUnits()).toBe(DEFAULT_XBE_DECLARED_SIZE);
		session.free();
	});
});

describe('ConversionSession(god source) => god', () => {
	const iso = makeFixture({ titleId: 0x6a6a0023 });
	let godParts: ReturnType<typeof convertXisoFixtureToGodParts>['dataParts'];

	beforeAll(() => {
		({ dataParts: godParts } = convertXisoFixtureToGodParts(iso, {
			format: 'god',
		}));
	});

	it('opens and drains without throwing', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'god' },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		driveHashing(session);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	// Unlike extracted=>extracted, god=>god is never rejected - a GOD image is
	// a genuine ImageSource (GodSource::open + remap_sector), so re-scrubbing
	// or re-authoring an already-GOD source is a real, meaningful operation
	// (e.g. re-running with a different `mode`), not a no-op.
	it('is accepted, unlike an extracted=>extracted target which is rejected', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'god' },
				{ source: GOD_SOURCE_OPTIONS, parts: godParts },
			),
		).not.toThrow();
	});

	it('outputManifest keeps the same <titleId>/<mediaId>/<contentType> naming shape as a god target built from xiso', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'god' },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		const manifest = session.outputManifest();
		session.free();
		const partEntries = manifest.slice(0, -1);
		for (const entry of partEntries) {
			expect(entry.name).toMatch(
				/^[0-9A-F]{8}\/[0-9A-F]{8}\/[0-9A-F]{8}\.data\/Data\d{4}$/,
			);
			expect(entry.size).toBeGreaterThan(0);
		}
		const header = manifest[manifest.length - 1];
		expect(header.name).toMatch(/^[0-9A-F]{8}\/[0-9A-F]{8}\/[0-9A-F]{8}$/);
	});

	// This is testing how god-as-source behaves when re-targeted at god, not
	// god's own target-side mode option - it stays here rather than moving to
	// mode-option.test.ts, which covers god purely as a target from xiso.
	it.each(['none', 'partial', 'full'] as const)(
		'mode: "%s" is accepted for a god source re-targeted at god, same as it is from an xiso source',
		(mode) => {
			const session = ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'god', mode },
				{ source: GOD_SOURCE_OPTIONS, parts: godParts },
			);
			expect(session.totalUnits()).toBeGreaterThanOrEqual(1);
			session.free();
		},
	);

	// Chains a second god=>god pass on top of the first, to exercise
	// GodSource::open reading a GOD image whose own content was produced by
	// another GOD target pass (not just by xiso) - proves the identity
	// (titleId) survives being remapped through GodSource twice in a row.
	it('titleId survives being re-derived from an already-god-sourced god output (double round trip)', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'god' },
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		const chunks: Uint8Array[] = [];
		while (!session.isDone()) {
			const chunk = session.nextChunk(1024 * 1024);
			if (chunk) chunks.push(chunk);
		}
		session.free();
		const all = concat(chunks);
		let cursor = 0;
		const rebuiltParts = manifest.slice(0, -1).map(({ name, size }) => {
			const bytes = all.slice(cursor, cursor + size);
			cursor += size;
			return { name, size: bytes.length, readFn: makeReadFn(bytes) };
		});
		const rebuiltTotalSize = rebuiltParts.reduce((n, p) => n + p.size, 0);
		const original = inspectSource(nullReadFn, iso.length, {
			source: { format: 'god' },
			parts: godParts,
		});
		const rebuilt = inspectSource(nullReadFn, rebuiltTotalSize, {
			source: { format: 'god' },
			parts: rebuiltParts,
		});
		expect(rebuilt.titleId).toBe(original.titleId);
		expect(rebuilt.contentType).toBe(original.contentType);
	});
});
