import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from './utils/wasm-setup.js';
import {
	makeFixture,
	DEFAULT_XBE_DECLARED_SIZE,
} from './utils/fixtures/xsf.js';
import {
	makeStfsMultiBlockListingFixture,
	makeStfsFixture,
} from './utils/fixtures/stfs.js';
import { inspectSource } from '../dist/index.js';
import { makeReadFn } from './utils/read-fns.js';
import {
	convertXisoFixtureToBytes,
	convertXisoFixtureToGodParts,
	convertXisoFixtureToExtractedParts,
} from './utils/session-helpers.js';
import {
	CCI_SOURCE,
	CISO_SOURCE,
	STFS_SOURCE,
	XISO_SOURCE,
	ZAR_SOURCE,
} from './utils/sources.js';

beforeAll(setupWasm);

// Checks BOTH e.name and e.message: a real wasm trap can surface as
// e.name === "RuntimeError" with e.message as short as "unreachable" (no
// "executed" suffix) - a message-only check for "unreachable executed"
// misses this exact shape, so both fields are checked.
const PANIC_SIGNATURE = /unreachable|RuntimeError|panicked at|wasm trap/i;

/** Success or a clean thrown Error both pass; only a PANIC_SIGNATURE match (in name or message) fails. */
function expectNoPanic(fn: () => void): void {
	try {
		fn();
	} catch (e) {
		expect(e).toBeInstanceOf(Error);
		const err = e as Error;
		expect(err.message).not.toMatch(PANIC_SIGNATURE);
		expect(err.name).not.toMatch(PANIC_SIGNATURE);
	}
}

describe('malformed input never panics (only ever a clean Error or success)', () => {
	describe('xiso: corrupted root directory table / XBE stub', () => {
		// Layout: dir table @ sector 0x21, XBE stub @ 0x22 (see xsf.ts).
		const DIR_TABLE_OFFSET = 0x21 * 0x800;
		const REGION_END = 0x22 * 0x800 + DEFAULT_XBE_DECLARED_SIZE;

		it('single-byte corruption anywhere in the directory table + XBE stub region', () => {
			const base = makeFixture({ titleId: 0x58490001 });
			for (let offset = DIR_TABLE_OFFSET; offset < REGION_END; offset++) {
				const mutated = base.slice();
				mutated[offset] ^= 0xff;
				expectNoPanic(() =>
					inspectSource(makeReadFn(mutated), mutated.length, XISO_SOURCE),
				);
			}
		}, 20000);

		it('truncation at various points', () => {
			const base = makeFixture({ titleId: 0x58490002 });
			const truncationPoints = [
				0,
				1,
				0x10000,
				0x10800,
				0x10801,
				0x11000,
				0x11200,
				base.length - 1,
			];
			for (const point of truncationPoints) {
				const truncated = base.slice(0, point);
				expectNoPanic(() =>
					inspectSource(makeReadFn(truncated), truncated.length, XISO_SOURCE),
				);
			}
		});

		it('a two-node BST pointer cycle in the root directory table does not panic or hang', () => {
			// Two entries (default.xbe + $SystemUpdate) so there's a real
			// second node to cycle with.
			const base = makeFixture({
				titleId: 0x58490003,
				includeSystemUpdate: true,
			});
			// entry1 starts right after entry0: entry0's on-disk size is
			// 14 (fixed fields) + name.length, rounded up to a 4-byte
			// boundary (writeDirectoryEntry in xsf.ts). 'default.xbe' is
			// 11 bytes, so entry0 spans 14 + 11 = 25 -> aligned to 28.
			const ENTRY0_NAME_LENGTH = 'default.xbe'.length;
			const ENTRY0_SIZE = Math.ceil((14 + ENTRY0_NAME_LENGTH) / 4) * 4;
			const ENTRY0_RIGHT = DIR_TABLE_OFFSET + 2;
			const ENTRY1_LEFT = DIR_TABLE_OFFSET + ENTRY0_SIZE;
			const ENTRY1_RIGHT = DIR_TABLE_OFFSET + ENTRY0_SIZE + 2;

			function setU16(buf: Uint8Array, offset: number, value: number): void {
				buf[offset] = value & 0xff;
				buf[offset + 1] = (value >> 8) & 0xff;
			}

			const mutated = base.slice();
			setU16(mutated, ENTRY0_RIGHT, 7); // entry0 -> entry1
			setU16(mutated, ENTRY1_LEFT, 7); // entry1 -> entry1 (self)
			setU16(mutated, ENTRY1_RIGHT, 0); // entry1 -> entry0

			const start = Date.now();
			expectNoPanic(() =>
				inspectSource(makeReadFn(mutated), mutated.length, XISO_SOURCE),
			);
			expect(Date.now() - start).toBeLessThan(2000);
		});
	});

	describe('stfs: corrupted file-table block chain', () => {
		it('a two-node next_block cycle in the file-table block chain does not panic or hang', () => {
			// Block B normally terminates (80 ff ff ff = status 0x80,
			// next_block = CHAIN_TERMINATOR); rewrite it to point back at
			// block A, making a real cycle in the chain the reader follows.
			const base = makeStfsMultiBlockListingFixture({});

			function findTerminatorEntries(buf: Uint8Array): number[] {
				const matches: number[] = [];
				for (let i = 0; i + 4 <= buf.length; i++) {
					if (
						buf[i] === 0x80 &&
						buf[i + 1] === 0xff &&
						buf[i + 2] === 0xff &&
						buf[i + 3] === 0xff
					) {
						matches.push(i);
					}
				}
				return matches;
			}

			const terminators = findTerminatorEntries(base);
			// Fails loudly if the fixture layout shifts, instead of
			// silently mutating the wrong bytes.
			expect(terminators.length).toBe(2);
			const blockBEntry = terminators[0];

			const mutated = base.slice();
			mutated[blockBEntry + 1] = 0x00;
			mutated[blockBEntry + 2] = 0x00;
			mutated[blockBEntry + 3] = 0x00;

			const start = Date.now();
			expectNoPanic(() =>
				inspectSource(makeReadFn(mutated), mutated.length, STFS_SOURCE),
			);
			expect(Date.now() - start).toBeLessThan(2000);
		});
	});

	describe('cci: corrupted single-blob output', () => {
		it('single-byte corruption at every offset', () => {
			const xiso = makeFixture({ titleId: 0x58490004 });
			const cci = convertXisoFixtureToBytes(xiso, {
				format: 'cci',
				outputName: 'out',
			});
			for (let offset = 0; offset < cci.length; offset++) {
				const mutated = cci.slice();
				mutated[offset] ^= 0xff;
				expectNoPanic(() =>
					inspectSource(makeReadFn(mutated), mutated.length, CCI_SOURCE),
				);
			}
		}, 20000);

		it('truncation at various points', () => {
			const xiso = makeFixture({ titleId: 0x58490005 });
			const cci = convertXisoFixtureToBytes(xiso, {
				format: 'cci',
				outputName: 'out',
			});
			const points = [0, 1, Math.floor(cci.length / 2), cci.length - 1];
			for (const point of points) {
				const truncated = cci.slice(0, point);
				expectNoPanic(() =>
					inspectSource(makeReadFn(truncated), truncated.length, CCI_SOURCE),
				);
			}
		});
	});

	describe('ciso: corrupted single-blob output', () => {
		it('single-byte corruption at every offset', () => {
			const xiso = makeFixture({ titleId: 0x58490006 });
			const ciso = convertXisoFixtureToBytes(xiso, {
				format: 'ciso',
				outputName: 'out',
			});
			for (let offset = 0; offset < ciso.length; offset++) {
				const mutated = ciso.slice();
				mutated[offset] ^= 0xff;
				expectNoPanic(() =>
					inspectSource(makeReadFn(mutated), mutated.length, CISO_SOURCE),
				);
			}
		}, 20000);

		it('truncation at various points', () => {
			const xiso = makeFixture({ titleId: 0x58490007 });
			const ciso = convertXisoFixtureToBytes(xiso, {
				format: 'ciso',
				outputName: 'out',
			});
			const points = [0, 1, Math.floor(ciso.length / 2), ciso.length - 1];
			for (const point of points) {
				const truncated = ciso.slice(0, point);
				expectNoPanic(() =>
					inspectSource(makeReadFn(truncated), truncated.length, CISO_SOURCE),
				);
			}
		});
	});

	describe('zar: corrupted single-blob output', () => {
		it('single-byte corruption at every offset', () => {
			const xiso = makeFixture({ titleId: 0x5a410002 });
			const zar = convertXisoFixtureToBytes(xiso, {
				format: 'zar',
				outputName: 'out',
			});
			for (let offset = 0; offset < zar.length; offset++) {
				const mutated = zar.slice();
				mutated[offset] ^= 0xff;
				expectNoPanic(() =>
					inspectSource(makeReadFn(mutated), mutated.length, ZAR_SOURCE),
				);
			}
		}, 20000);

		it('truncation at various points', () => {
			const xiso = makeFixture({ titleId: 0x5a410003 });
			const zar = convertXisoFixtureToBytes(xiso, {
				format: 'zar',
				outputName: 'out',
			});
			const points = [0, 1, Math.floor(zar.length / 2), zar.length - 1];
			for (const point of points) {
				const truncated = zar.slice(0, point);
				expectNoPanic(() =>
					inspectSource(makeReadFn(truncated), truncated.length, ZAR_SOURCE),
				);
			}
		});
	});

	describe('god: corrupted multi-part output', () => {
		it('single-byte corruption at every offset, in every part', () => {
			const xiso = makeFixture({ titleId: 0x58490008 });
			const { dataParts, headerPart } = convertXisoFixtureToGodParts(xiso);
			const allParts = [...dataParts, headerPart];

			for (const target of allParts) {
				const originalBytes = target.readFn(0, target.size);
				for (let offset = 0; offset < originalBytes.length; offset++) {
					const mutated = originalBytes.slice();
					mutated[offset] ^= 0xff;
					const parts = allParts.map((p) =>
						p === target
							? {
									name: p.name,
									size: mutated.length,
									readFn: makeReadFn(mutated),
								}
							: p,
					);
					expectNoPanic(() =>
						inspectSource(() => new Uint8Array(0), xiso.length, {
							source: { format: 'god' },
							parts,
						}),
					);
				}
			}
		}, 30000);

		it('truncation of each part', () => {
			const xiso = makeFixture({ titleId: 0x58490009 });
			const { dataParts, headerPart } = convertXisoFixtureToGodParts(xiso);
			const allParts = [...dataParts, headerPart];

			for (const target of allParts) {
				const originalBytes = target.readFn(0, target.size);
				const points = [0, 1, Math.floor(originalBytes.length / 2)];
				for (const point of points) {
					const truncated = originalBytes.slice(0, point);
					const parts = allParts.map((p) =>
						p === target
							? {
									name: p.name,
									size: truncated.length,
									readFn: makeReadFn(truncated),
								}
							: p,
					);
					expectNoPanic(() =>
						inspectSource(() => new Uint8Array(0), xiso.length, {
							source: { format: 'god' },
							parts,
						}),
					);
				}
			}
		});
	});

	describe('extracted: corrupted loose-file output', () => {
		it('single-byte corruption at every offset', () => {
			const xiso = makeFixture({ titleId: 0x5849000a });
			const parts = convertXisoFixtureToExtractedParts(xiso);

			for (const target of parts) {
				const originalBytes = target.readFn(0, target.size);
				for (let offset = 0; offset < originalBytes.length; offset++) {
					const mutated = originalBytes.slice();
					mutated[offset] ^= 0xff;
					const newParts = parts.map((p) =>
						p === target
							? {
									name: p.name,
									size: mutated.length,
									readFn: makeReadFn(mutated),
								}
							: p,
					);
					expectNoPanic(() =>
						inspectSource(() => new Uint8Array(0), xiso.length, {
							source: { format: 'extracted' },
							parts: newParts,
						}),
					);
				}
			}
		}, 20000);

		it('truncation of each part', () => {
			const xiso = makeFixture({ titleId: 0x5849000b });
			const parts = convertXisoFixtureToExtractedParts(xiso);

			for (const target of parts) {
				const originalBytes = target.readFn(0, target.size);
				const points = [0, 1, Math.floor(originalBytes.length / 2)];
				for (const point of points) {
					const truncated = originalBytes.slice(0, point);
					const newParts = parts.map((p) =>
						p === target
							? {
									name: p.name,
									size: truncated.length,
									readFn: makeReadFn(truncated),
								}
							: p,
					);
					expectNoPanic(() =>
						inspectSource(() => new Uint8Array(0), xiso.length, {
							source: { format: 'extracted' },
							parts: newParts,
						}),
					);
				}
			}
		});
	});

	describe('shared launch-executable size overflow (zar / stfs / extracted)', () => {
		it('zar: an inflated size_low field on the file-tree entry for default.xbe', () => {
			const xiso = makeFixture({ titleId: 0x5a410001 });
			const zar = convertXisoFixtureToBytes(xiso, {
				format: 'zar',
				outputName: 'out',
			});

			// footer -> file_tree SectionInfo -> entry 1 (default.xbe,
			// since there's no $SystemUpdate). size_low is a big-endian
			// u32 at entry bytes 8..12 (zar/read.rs's TreeNodeRaw::File).
			const FOOTER_SIZE = 6 * 16 + 32 + 8 + 4 + 4;
			const footerStart = zar.length - FOOTER_SIZE;
			const view = new DataView(zar.buffer, zar.byteOffset, zar.byteLength);
			const fileTreeOffset = Number(
				view.getBigUint64(footerStart + 3 * 16, false),
			);
			const sizeLowOffset = fileTreeOffset + 16 + 8;

			const mutated = zar.slice();
			mutated[sizeLowOffset] = 0xff; // MSB of size_low

			expectNoPanic(() =>
				inspectSource(makeReadFn(mutated), mutated.length, {
					source: { format: 'zar' },
				}),
			);
		});

		it('stfs: an inflated fileSize field on the file-table entry', () => {
			const { bytes, fileTableAddr } = makeStfsFixture({ titleId: 0x53540001 });
			const mutated = bytes.slice();
			const view = new DataView(mutated.buffer);
			// fileSize: big-endian u32 at +0x34 (see makeStfsFixture).
			view.setUint32(fileTableAddr + 0x34, 0xffffffff, false);

			expectNoPanic(() =>
				inspectSource(makeReadFn(mutated), mutated.length, {
					source: { format: 'stfs' },
				}),
			);
		});

		it("extracted: a SourcePart.size that doesn't match the file's real byte count", () => {
			const xbeBytes = new Uint8Array(DEFAULT_XBE_DECLARED_SIZE);
			const parts = [
				{
					name: 'default.xbe',
					size: 0xffffffff, // never checked against xbeBytes.length
					readFn: makeReadFn(xbeBytes),
				},
			];

			expectNoPanic(() =>
				inspectSource(() => new Uint8Array(0), xbeBytes.length, {
					source: { format: 'extracted' },
					parts,
				}),
			);
		});
	});

	// The wasm module is a file-level singleton (wasm-setup.ts), so a bad
	// input and the next call genuinely share linear memory here. These
	// reuse the three inputs above - clean errors now, not panics, since
	// the extracted_fs.rs fix - to check a thrown Error never corrupts
	// module state for the next caller.
	describe('module keeps working after a handled Error from malformed input', () => {
		it('zar: a legitimate call after the inflated size_low input still works', () => {
			const xiso = makeFixture({ titleId: 0x5a410004 });
			const zar = convertXisoFixtureToBytes(xiso, {
				format: 'zar',
				outputName: 'out',
			});
			const FOOTER_SIZE = 6 * 16 + 32 + 8 + 4 + 4;
			const footerStart = zar.length - FOOTER_SIZE;
			const view = new DataView(zar.buffer, zar.byteOffset, zar.byteLength);
			const fileTreeOffset = Number(
				view.getBigUint64(footerStart + 3 * 16, false),
			);
			const sizeLowOffset = fileTreeOffset + 16 + 8;

			const mutated = zar.slice();
			mutated[sizeLowOffset] = 0xff;
			expectNoPanic(() =>
				inspectSource(makeReadFn(mutated), mutated.length, ZAR_SOURCE),
			);

			const freshXiso = makeFixture({ titleId: 0x5a410005 });
			const freshZar = convertXisoFixtureToBytes(freshXiso, {
				format: 'zar',
				outputName: 'out',
			});
			const info = inspectSource(
				makeReadFn(freshZar),
				freshZar.length,
				ZAR_SOURCE,
			);
			expect(info.titleId).toBe('5A410005');
		});

		it('stfs: a legitimate call after the inflated fileSize input still works', () => {
			const { bytes, fileTableAddr } = makeStfsFixture({
				titleId: 0x53540002,
			});
			const mutated = bytes.slice();
			const view = new DataView(mutated.buffer);
			view.setUint32(fileTableAddr + 0x34, 0xffffffff, false);
			expectNoPanic(() =>
				inspectSource(makeReadFn(mutated), mutated.length, STFS_SOURCE),
			);

			const fresh = makeStfsFixture({ titleId: 0x53540003 });
			const info = inspectSource(
				makeReadFn(fresh.bytes),
				fresh.bytes.length,
				STFS_SOURCE,
			);
			expect(info.titleId).toBe('53540003');
		});

		it('extracted: a legitimate call after the mismatched SourcePart.size input still works', () => {
			const badXbeBytes = new Uint8Array(DEFAULT_XBE_DECLARED_SIZE);
			const badParts = [
				{
					name: 'default.xbe',
					size: 0xffffffff,
					readFn: makeReadFn(badXbeBytes),
				},
			];
			expectNoPanic(() =>
				inspectSource(() => new Uint8Array(0), badXbeBytes.length, {
					source: { format: 'extracted' },
					parts: badParts,
				}),
			);

			const freshXiso = makeFixture({ titleId: 0x45580001 });
			const freshParts = convertXisoFixtureToExtractedParts(freshXiso);
			const info = inspectSource(() => new Uint8Array(0), freshXiso.length, {
				source: { format: 'extracted' },
				parts: freshParts,
			});
			expect(info.titleId).toBe('45580001');
		});
	});
});
