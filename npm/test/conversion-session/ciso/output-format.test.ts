import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { makeReadFn } from '../../utils/read-fns.js';
import { drain, driveHashing } from '../../utils/session-helpers.js';
import {
	ConversionSession,
	cisoFilePaddingModulus,
	cisoSectorSize,
} from '../../../dist/index.js';
import { XISO_SOURCE } from '../../utils/sources.js';

let SECTOR_SIZE: number;
let FILE_PADDING_MODULUS: number;
beforeAll(async () => {
	await setupWasm();
	SECTOR_SIZE = cisoSectorSize();
	FILE_PADDING_MODULUS = cisoFilePaddingModulus();
});

const OUTPUT_NAME = 'test';

// The CSO header is a fixed 24-byte, packed, little-endian struct written
// verbatim by `hash_next_part()` once sizing completes. These checks read
// the raw bytes of the first chunk directly rather than going through any
// higher-level API, so they'll catch a field-order or endianness regression
// in the (private, hand-rolled) index-table serialization that no other
// test here would notice.
describe('ConversionSession(ciso) output header format', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);
	it('starts with the "CISO" magic, version 2, and a 24-byte header size', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		// Capture before nextChunk()/free() - totalUnits() is only
		// meaningful pre-consumption.
		const totalUnits = session.totalUnits();
		const header = session.nextChunk(24)!;
		session.free();
		expect(header.length).toBeGreaterThanOrEqual(24);
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		const magic = String.fromCharCode(
			view.getUint8(0),
			view.getUint8(1),
			view.getUint8(2),
			view.getUint8(3),
		);
		expect(magic).toBe('CISO');
		expect(view.getUint32(4, true)).toBe(24);
		// uncompressed_size reflects the repacked (real-content) image size,
		// i.e. totalUnits() * SECTOR_SIZE - not the raw fixture's byte length.
		expect(view.getBigUint64(8, true)).toBe(BigInt(totalUnits * SECTOR_SIZE));
		expect(view.getUint32(16, true)).toBe(SECTOR_SIZE);
		expect(view.getUint8(20)).toBe(2);
	});

	it('pads the final output to a multiple of 0x400 bytes (FILE_PADDING_MODULUS)', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		const out = drain(session, 64 * SECTOR_SIZE);
		expect(out.length).toBe(manifest[0].size);
		expect(out.length % FILE_PADDING_MODULUS).toBe(0);
	});
});
