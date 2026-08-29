import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import {
	DEFAULT_XBE_DECLARED_SIZE,
	makeFixture,
} from '../../utils/fixtures/xsf.js';
import { ConversionSession } from '../../../dist/index.js';
import { makeReadFn, nullReadFn } from '../../utils/read-fns.js';
import {
	convertXisoFixtureToBytes,
	convertXisoFixtureToExtractedParts,
	convertXisoFixtureToGodParts,
	drain,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import {
	XISO_SOURCE,
	EXTRACTED_SOURCE,
	CISO_SOURCE,
	CCI_SOURCE,
	GOD_SOURCE,
} from '../../utils/sources.js';

beforeAll(setupWasm);

const OUTPUT_NAME = 'game';

// ZarSession has two entry points - open() (from an already-open image
// source, e.g. xiso/ciso/cci/god) and open_from_extracted() (from a loose
// extracted-files directory) - that funnel into the same build() step (see
// the module doc comment in formats/zar.rs). Every test above this file
// only exercises the image-source path via `XISO_SOURCE`; these confirm
// the extracted-files path works too, and that the two really do converge
// on the same archive for equivalent content.
describe('ConversionSession(zar) from an extracted-files source', () => {
	const iso = makeFixture({ titleId: 0x41560001 });

	it('opens and drains without throwing', () => {
		const parts = convertXisoFixtureToExtractedParts(iso);
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'zar', outputName: OUTPUT_NAME },
			{ ...EXTRACTED_SOURCE, parts },
		);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	it("totalUnits matches the file's declared size, same as going through the image source", () => {
		const parts = convertXisoFixtureToExtractedParts(iso);
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'zar', outputName: OUTPUT_NAME },
			{ ...EXTRACTED_SOURCE, parts },
		);
		expect(session.totalUnits()).toBe(DEFAULT_XBE_DECLARED_SIZE);
		session.free();
	});

	it('produces byte-identical output to converting the same content straight from the xiso image', () => {
		const parts = convertXisoFixtureToExtractedParts(iso);
		const fromExtracted = drain(
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'zar', outputName: OUTPUT_NAME },
				{ ...EXTRACTED_SOURCE, parts },
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		const fromImage = drain(
			ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'zar', outputName: OUTPUT_NAME },
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		expect(fromExtracted).toEqual(fromImage);
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'zar', outputName: OUTPUT_NAME },
				{ ...EXTRACTED_SOURCE, parts: [] },
			),
		).toThrow();
	});
});

// ---------------------------------------------------------------------------
// The image-source path - ZarSession::open() over an already-open
// ImageSource - is only ever exercised via XISO_SOURCE elsewhere in this
// suite. ciso/cci/god are each a *different* ImageSource implementation
// (CisoSource/CciSource/GodSource, not XisoSource), so each is worth its
// own round trip: does zar's build() step walk the same XDVDFS tree the
// same way regardless of which concrete ImageSource is handing it sectors?
// ---------------------------------------------------------------------------

describe('ConversionSession(zar) from a ciso source', () => {
	const iso = makeFixture({ titleId: 0x41560002 });
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
			{ format: 'zar', outputName: OUTPUT_NAME },
			CISO_SOURCE,
		);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	it("totalUnits matches the file's declared size, same as going through the xiso image directly", () => {
		const session = ConversionSession.open(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			{ format: 'zar', outputName: OUTPUT_NAME },
			CISO_SOURCE,
		);
		expect(session.totalUnits()).toBe(DEFAULT_XBE_DECLARED_SIZE);
		session.free();
	});

	it('produces byte-identical output to converting the same content straight from the xiso image', () => {
		const fromCiso = drain(
			ConversionSession.open(
				makeReadFn(cisoBytes),
				cisoBytes.length,
				{ format: 'zar', outputName: OUTPUT_NAME },
				CISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		const fromImage = drain(
			ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'zar', outputName: OUTPUT_NAME },
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
				{ format: 'zar', outputName: OUTPUT_NAME },
				{ ...CISO_SOURCE, parts: [] },
			),
		).toThrow();
	});
});

describe('ConversionSession(zar) from a cci source', () => {
	const iso = makeFixture({ titleId: 0x41560003 });
	let cciBytes: Uint8Array;
	beforeAll(() => {
		cciBytes = convertXisoFixtureToBytes(iso, {
			format: 'cci',
			outputName: 'src',
		});
	});

	it('opens and drains without throwing', () => {
		const session = ConversionSession.open(
			makeReadFn(cciBytes),
			cciBytes.length,
			{ format: 'zar', outputName: OUTPUT_NAME },
			CCI_SOURCE,
		);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	it("totalUnits matches the file's declared size, same as going through the xiso image directly", () => {
		const session = ConversionSession.open(
			makeReadFn(cciBytes),
			cciBytes.length,
			{ format: 'zar', outputName: OUTPUT_NAME },
			CCI_SOURCE,
		);
		expect(session.totalUnits()).toBe(DEFAULT_XBE_DECLARED_SIZE);
		session.free();
	});

	it('produces byte-identical output to converting the same content straight from the xiso image', () => {
		const fromCci = drain(
			ConversionSession.open(
				makeReadFn(cciBytes),
				cciBytes.length,
				{ format: 'zar', outputName: OUTPUT_NAME },
				CCI_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		const fromImage = drain(
			ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'zar', outputName: OUTPUT_NAME },
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		expect(fromCci).toEqual(fromImage);
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				cciBytes.length,
				{ format: 'zar', outputName: OUTPUT_NAME },
				{ ...CCI_SOURCE, parts: [] },
			),
		).toThrow();
	});
});

describe('ConversionSession(zar) from a god source', () => {
	const iso = makeFixture({ titleId: 0x41560004 });
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
			{ format: 'zar', outputName: OUTPUT_NAME },
			{ ...GOD_SOURCE, parts: godParts },
		);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		session.free();
	});

	it("totalUnits matches the file's declared size, same as going through the xiso image directly", () => {
		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'zar', outputName: OUTPUT_NAME },
			{ ...GOD_SOURCE, parts: godParts },
		);
		expect(session.totalUnits()).toBe(DEFAULT_XBE_DECLARED_SIZE);
		session.free();
	});

	it('produces byte-identical output to converting the same content straight from the xiso image', () => {
		const fromGod = drain(
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'zar', outputName: OUTPUT_NAME },
				{ ...GOD_SOURCE, parts: godParts },
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		const fromImage = drain(
			ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'zar', outputName: OUTPUT_NAME },
				XISO_SOURCE,
			),
			UNBOUNDED_CHUNK_SIZE,
		);
		expect(fromGod).toEqual(fromImage);
	});

	// Unlike ciso/cci/extracted (which fall back to a single implicit part
	// built from readFn/fileSize when sourceParts is omitted), a god source
	// has no meaningful single-part shape - see the identical check in
	// god/conversion.test.ts's "ConversionSession(god source) error paths".
	it('throws when sourceParts is omitted for a god source, instead of falling back to a single-part read', () => {
		expect(() =>
			ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'zar', outputName: OUTPUT_NAME },
				GOD_SOURCE,
			),
		).toThrow();
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				iso.length,
				{ format: 'zar', outputName: OUTPUT_NAME },
				{ ...GOD_SOURCE, parts: [] },
			),
		).toThrow();
	});
});
