import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from './utils/wasm-setup.js';
import {
	makeFixture,
	DEFAULT_XBE_DECLARED_SIZE,
} from './utils/fixtures/xsf.js';
import { generateAttachXbe } from '../dist/index.js';
import { makeReadFn, nullReadFn, throwingReadFn } from './utils/read-fns.js';
import { convertXisoFixtureToExtractedParts } from './utils/session-helpers.js';
import { readU32LE, certAddr } from './utils/fixtures/binary-utils.js';
import { XISO_SOURCE, EXTRACTED_SOURCE_OPTIONS } from './utils/sources.js';

beforeAll(setupWasm);

// Mirrors core::attach_xbe's private CERT_TITLE_ID_OFFSET/
// CERT_TITLE_NAME_OFFSET.
const CERT_TITLE_ID_OFFSET = 0x08;
const CERT_TITLE_NAME_OFFSET = 0x0c;

function fixture(opts: { titleId: number; platform?: 'ogx' | 'x360' }) {
	return makeFixture({
		...opts,
		xbeDeclaredSize: DEFAULT_XBE_DECLARED_SIZE,
	});
}

describe('generateAttachXbe - basic shape', () => {
	it('returns a Uint8Array starting with the XBE magic', () => {
		const iso = fixture({ titleId: 0x41560001 });
		const out = generateAttachXbe(makeReadFn(iso), iso.length, XISO_SOURCE);
		expect(out).toBeInstanceOf(Uint8Array);
		expect(new TextDecoder().decode(out.slice(0, 4))).toBe('XBEH');
	});

	it("copies the source's titleId into the returned stub's certificate", () => {
		const iso = fixture({ titleId: 0x41560001 });
		const out = generateAttachXbe(makeReadFn(iso), iso.length, XISO_SOURCE);
		const addr = certAddr(out);
		expect(readU32LE(out, addr + CERT_TITLE_ID_OFFSET)).toBe(0x41560001);
	});

	it('produces stubs with different titleIds for different source fixtures', () => {
		const isoA = fixture({ titleId: 0x41560001 });
		const isoB = fixture({ titleId: 0xdeadbeef });
		const outA = generateAttachXbe(makeReadFn(isoA), isoA.length, XISO_SOURCE);
		const outB = generateAttachXbe(makeReadFn(isoB), isoB.length, XISO_SOURCE);
		const addr = certAddr(outA);
		expect(readU32LE(outA, addr + CERT_TITLE_ID_OFFSET)).not.toBe(
			readU32LE(outB, addr + CERT_TITLE_ID_OFFSET),
		);
	});

	it("copies the source's title_name field into the returned stub's certificate", () => {
		// xsf.ts's default.xbe stub never writes real bytes into
		// title_name, so this only confirms build_attach_xbe's
		// unconditional title_name copy runs, not that it produces a
		// particular string. See xbe-patch.test.ts for a fixture with a
		// real, non-zero title_name via renameTitle.
		const iso = fixture({ titleId: 0x41560001 });
		const out = generateAttachXbe(makeReadFn(iso), iso.length, XISO_SOURCE);
		const addr = certAddr(out);
		const nameBytes = out.slice(
			addr + CERT_TITLE_NAME_OFFSET,
			addr + CERT_TITLE_NAME_OFFSET + 80,
		);
		expect(nameBytes.every((b) => b === 0)).toBe(true);
	});

	it('is deterministic for the same source fixture', () => {
		const iso = fixture({ titleId: 0x88880001 });
		const out1 = generateAttachXbe(makeReadFn(iso), iso.length, XISO_SOURCE);
		const out2 = generateAttachXbe(makeReadFn(iso), iso.length, XISO_SOURCE);
		expect(out1).toEqual(out2);
	});
});

describe('generateAttachXbe - OGX-only gate', () => {
	it('rejects an image source whose launch executable is an Xbox 360 XEX', () => {
		const iso = fixture({ titleId: 0x5a5a0001, platform: 'x360' });
		expect(() =>
			generateAttachXbe(makeReadFn(iso), iso.length, XISO_SOURCE),
		).toThrow(/OGX sources|GoD \(XEX\)/);
	});

	it('accepts an OGX image source (platform: "ogx", the default)', () => {
		const iso = fixture({ titleId: 0x5a5a0002, platform: 'ogx' });
		expect(() =>
			generateAttachXbe(makeReadFn(iso), iso.length, XISO_SOURCE),
		).not.toThrow();
	});
});

describe('generateAttachXbe - error paths', () => {
	it('throws for a zeroed (invalid) image', () => {
		expect(() =>
			generateAttachXbe(nullReadFn, 10 * 1024 * 1024, XISO_SOURCE),
		).toThrow();
	});

	it('propagates errors thrown inside readFn', () => {
		expect(() =>
			generateAttachXbe(throwingReadFn, 10 * 1024 * 1024, XISO_SOURCE),
		).toThrow('read error from JS');
	});

	it('throws when source is omitted, instead of silently assuming xiso', () => {
		const iso = fixture({ titleId: 0x41560001 });
		expect(() =>
			// @ts-expect-error - source is intentionally omitted to assert
			// the runtime contract for callers not using TS.
			generateAttachXbe(makeReadFn(iso), iso.length, undefined),
		).toThrow(/source format must be resolved/);
	});
});

describe('generateAttachXbe - ExtractedFs source', () => {
	// Exercises the SourceInner::ExtractedFs branch of generate_attach_xbe
	// (via read_launch_executable()) rather than the Image/XDVDFS-walk
	// branch every test above uses.
	it('works from a set of already-extracted files, not just a raw image', () => {
		const iso = fixture({ titleId: 0x77770001 });
		const parts = convertXisoFixtureToExtractedParts(iso);
		const out = generateAttachXbe(nullReadFn, iso.length, {
			source: EXTRACTED_SOURCE_OPTIONS,
			parts,
		});
		const addr = certAddr(out);
		expect(readU32LE(out, addr + CERT_TITLE_ID_OFFSET)).toBe(0x77770001);
	});

	it('rejects an extracted source whose launch executable is a XEX (x360)', () => {
		const iso = fixture({ titleId: 0x77770002, platform: 'x360' });
		const parts = convertXisoFixtureToExtractedParts(iso);
		expect(() =>
			generateAttachXbe(nullReadFn, iso.length, {
				source: EXTRACTED_SOURCE_OPTIONS,
				parts,
			}),
		).toThrow(/OGX sources|GoD \(XEX\)/);
	});

	it('throws if sourceParts is empty for an extracted source', () => {
		expect(() =>
			generateAttachXbe(nullReadFn, 0, {
				source: EXTRACTED_SOURCE_OPTIONS,
				parts: [],
			}),
		).toThrow();
	});
});
