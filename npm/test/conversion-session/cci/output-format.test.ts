import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { makeReadFn } from '../../utils/read-fns.js';
import { driveHashing, drain } from '../../utils/session-helpers.js';
import { ConversionSession, cciSectorSize } from '../../../dist/index.js';
import { XISO_SOURCE } from '../../utils/sources.js';

let SECTOR_SIZE: number;
beforeAll(async () => {
	await setupWasm();
	SECTOR_SIZE = cciSectorSize();
});

const OUTPUT_NAME = 'test';

// The CCI header is a fixed 32-byte, packed, little-endian struct - 8 bytes
// larger than CISO's because of the explicit `index_offset` field (CCI's
// index trails the data instead of leading it, so readers need to be told
// where it starts). These checks read the raw bytes of the first chunk
// directly, the same way the CISO equivalent does, so they'll catch a
// field-order/endianness regression in the hand-rolled header serialization.
describe('ConversionSession(cci) output header format', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);
	it('starts with the "CCIM" magic, version 1, index alignment 2, and a 32-byte header size', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'cci',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		// Capture before nextChunk()/free() - totalUnits() is only
		// meaningful pre-consumption.
		const totalUnits = session.totalUnits();
		const header = session.nextChunk(32)!;
		session.free();
		expect(header.length).toBeGreaterThanOrEqual(32);
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		const magic = String.fromCharCode(
			view.getUint8(0),
			view.getUint8(1),
			view.getUint8(2),
			view.getUint8(3),
		);
		expect(magic).toBe('CCIM');
		expect(view.getUint32(4, true)).toBe(32);
		// uncompressed_size reflects the repacked (real-content) image size,
		// i.e. totalUnits() * SECTOR_SIZE - not the raw fixture's byte length.
		expect(view.getBigUint64(8, true)).toBe(BigInt(totalUnits * SECTOR_SIZE));
		// index_offset must land strictly after the header - it points at
		// this part's trailing index table, which can't start before the
		// header ends and the sector data begins.
		const indexOffset = view.getBigUint64(16, true);
		expect(indexOffset).toBeGreaterThan(32n);
		expect(view.getUint32(24, true)).toBe(SECTOR_SIZE);
		expect(view.getUint8(28)).toBe(1); // VERSION
		expect(view.getUint8(29)).toBe(2); // INDEX_ALIGNMENT
	});
	// Unlike CISO (one global index in the first split file only), every CCI
	// part is fully self-contained: header.index_offset must point exactly
	// at the byte where that part's own trailing index begins, and the
	// index must run to the very end of the file. This is the part of the
	// format CCI genuinely does differently from CISO, so it's worth
	// verifying end-to-end rather than only via the pure-arithmetic Rust
	// unit tests in cci.rs.
	it('index_offset points exactly at the trailing index table, which runs to end of file', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'cci',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		// Capture before drain(), which calls session.free() internally -
		// totalUnits() is only meaningful pre-consumption.
		const totalUnits = session.totalUnits();
		const out = drain(session, 64 * SECTOR_SIZE);
		expect(manifest).toHaveLength(1);
		const view = new DataView(out.buffer, out.byteOffset, out.length);
		const indexOffset = Number(view.getBigUint64(16, true));
		// index table = one u32 per sector, plus one trailing index_end
		// marker - see finalize_sizing_part in cci.rs.
		const sectorCount = (out.length - indexOffset) / 4 - 1;
		expect(Number.isInteger(sectorCount)).toBe(true);
		expect(sectorCount).toBe(totalUnits);
		expect(indexOffset + (sectorCount + 1) * 4).toBe(out.length);
		expect(out.length).toBe(manifest[0].size);
	});
});
