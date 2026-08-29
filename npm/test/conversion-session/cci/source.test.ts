import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { ConversionSession } from '../../../dist/index.js';
import { makeReadFn, nullReadFn } from '../../utils/read-fns.js';
import {
	driveHashing,
	drain,
	driveAndDrain,
	convertXisoFixtureToExtractedParts,
	convertXisoFixtureToBytes,
	convertXisoFixtureToGodParts,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import {
	XISO_SOURCE,
	EXTRACTED_SOURCE,
	CISO_SOURCE,
	ZAR_SOURCE,
	GOD_SOURCE,
} from '../../utils/sources.js';

beforeAll(setupWasm);

const OUTPUT_NAME = 'game';

describe('ConversionSession(cci) from an extracted-files source', () => {
	const iso = makeFixture({ titleId: 0x63630001 });
	let parts: ReturnType<typeof convertXisoFixtureToExtractedParts>;
	beforeAll(() => {
		parts = convertXisoFixtureToExtractedParts(iso);
	});

	it('opens and drains without throwing', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			{ ...EXTRACTED_SOURCE, parts },
		);
		driveHashing(session);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	it('outputManifest has a single "game.cci" entry, under the split threshold', () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			{ ...EXTRACTED_SOURCE, parts },
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		const out = drain(session, UNBOUNDED_CHUNK_SIZE);
		expect(manifest).toHaveLength(1);
		expect(manifest[0].name).toBe(`${OUTPUT_NAME}.cci`);
		expect(manifest[0].size).toBe(out.length);
	});

	// Rebuilding from extracted loose files produces a valid XDVDFS layout,
	// but file traversal and directory entry ordering can differ from a raw
	// XISO stream. Therefore, we verify that totalUnits matches the xiso
	// conversion rather than expecting strict byte-identical output.
	it('totalUnits matches converting the same fixture straight from the xiso image', () => {
		const fromExtracted = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			{ ...EXTRACTED_SOURCE, parts },
		);
		driveHashing(fromExtracted);
		const extractedUnits = fromExtracted.totalUnits();
		fromExtracted.free();
		const fromXiso = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			XISO_SOURCE,
		);
		driveHashing(fromXiso);
		const xisoUnits = fromXiso.totalUnits();
		fromXiso.free();
		expect(extractedUnits).toBeGreaterThan(0);
		expect(extractedUnits).toBe(xisoUnits);
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'cci', outputName: OUTPUT_NAME },
				{ ...EXTRACTED_SOURCE, parts: [] },
			),
		).toThrow();
	});
});

describe('ConversionSession(cci) from a zar source', () => {
	const iso = makeFixture({ titleId: 0x63630002 });
	let zarBytes: Uint8Array;
	beforeAll(() => {
		zarBytes = convertXisoFixtureToBytes(iso, {
			format: 'zar',
			outputName: 'src',
		});
	});

	it('opens and drains without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(zarBytes),
			zarBytes.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			ZAR_SOURCE,
		);
		driveHashing(session);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	it('totalUnits matches converting the same fixture straight from the xiso image', () => {
		const fromZar = ConversionSession.open(
			makeReadFn(zarBytes),
			zarBytes.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			ZAR_SOURCE,
		);
		driveHashing(fromZar);
		const zarUnits = fromZar.totalUnits();
		fromZar.free();
		const fromXiso = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			XISO_SOURCE,
		);
		driveHashing(fromXiso);
		const xisoUnits = fromXiso.totalUnits();
		fromXiso.free();
		expect(zarUnits).toBeGreaterThan(0);
		expect(zarUnits).toBe(xisoUnits);
	});

	it('totalUnits matches converting the same fixture straight from the xiso image', () => {
		const fromZar = ConversionSession.open(
			makeReadFn(zarBytes),
			zarBytes.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			ZAR_SOURCE,
		);
		driveHashing(fromZar);
		const zarUnits = fromZar.totalUnits();
		fromZar.free();
		const fromXiso = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			XISO_SOURCE,
		);
		driveHashing(fromXiso);
		const xisoUnits = fromXiso.totalUnits();
		fromXiso.free();
		expect(zarUnits).toBeGreaterThan(0);
		expect(zarUnits).toBe(xisoUnits);
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				zarBytes.length,
				{ format: 'cci', outputName: OUTPUT_NAME },
				{ ...ZAR_SOURCE, parts: [] },
			),
		).toThrow();
	});
});

describe('ConversionSession(cci) from a ciso source', () => {
	const iso = makeFixture({ titleId: 0x63630003 });
	let cisoBytes: Uint8Array;
	beforeAll(() => {
		cisoBytes = convertXisoFixtureToBytes(iso, {
			format: 'ciso',
			outputName: 'src',
		});
	});

	it('opens and drains without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			CISO_SOURCE,
		);
		driveHashing(session);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	it('totalUnits matches converting the same fixture straight from the xiso image', () => {
		const fromCiso = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			CISO_SOURCE,
		);
		driveHashing(fromCiso);
		const cisoUnits = fromCiso.totalUnits();
		fromCiso.free();
		const fromXiso = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			XISO_SOURCE,
		);
		driveHashing(fromXiso);
		const xisoUnits = fromXiso.totalUnits();
		fromXiso.free();
		expect(cisoUnits).toBeGreaterThan(0);
		expect(cisoUnits).toBe(xisoUnits);
	});

	it('produces byte-identical output to converting the same content straight from the xiso image', () => {
		const fromCiso = driveAndDrain(
			ConversionSession.open(
				makeReadFn(cisoBytes),
				cisoBytes.length,
				{ format: 'cci', outputName: OUTPUT_NAME },
				CISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		const fromImage = driveAndDrain(
			ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'cci', outputName: OUTPUT_NAME },
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		expect(fromCiso).toEqual(fromImage);
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				cisoBytes.length,
				{ format: 'cci', outputName: OUTPUT_NAME },
				{ ...CISO_SOURCE, parts: [] },
			),
		).toThrow();
	});
});

describe('ConversionSession(cci) from a god source', () => {
	const iso = makeFixture({ titleId: 0x63630004 });
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
			{ format: 'cci', outputName: OUTPUT_NAME },
			{ ...GOD_SOURCE, parts: godParts },
		);
		driveHashing(session);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	it('totalUnits matches converting the same fixture straight from the xiso image', () => {
		const fromGod = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			{ ...GOD_SOURCE, parts: godParts },
		);
		driveHashing(fromGod);
		const godUnits = fromGod.totalUnits();
		fromGod.free();
		const fromXiso = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'cci', outputName: OUTPUT_NAME },
			XISO_SOURCE,
		);
		driveHashing(fromXiso);
		const xisoUnits = fromXiso.totalUnits();
		fromXiso.free();
		expect(godUnits).toBeGreaterThan(0);
		expect(godUnits).toBe(xisoUnits);
	});

	it('produces byte-identical output to converting the same content straight from the xiso image', () => {
		const fromGod = driveAndDrain(
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'cci', outputName: OUTPUT_NAME },
				{ ...GOD_SOURCE, parts: godParts },
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		const fromImage = driveAndDrain(
			ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'cci', outputName: OUTPUT_NAME },
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		expect(fromGod).toEqual(fromImage);
	});

	it('throws when sourceParts is omitted for a god source, instead of falling back to a single-part read', () => {
		expect(() =>
			ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'cci', outputName: OUTPUT_NAME },
				GOD_SOURCE,
			),
		).toThrow();
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'cci', outputName: OUTPUT_NAME },
				{ ...GOD_SOURCE, parts: [] },
			),
		).toThrow();
	});
});
