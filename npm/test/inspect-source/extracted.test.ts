import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { inspectSource } from '../../dist/index.js';
import { makeReadFn, nullReadFn, throwingReadFn } from '../utils/read-fns.js';
import { convertXisoFixtureToExtractedParts } from '../utils/session-helpers.js';
import { EXTRACTED_SOURCE_OPTIONS as EXTRACTED_SOURCE } from '../utils/sources.js';

beforeAll(setupWasm);

describe('inspectSource with an `extracted` source', () => {
	// Extracted-source inspection reads default.xbe/default.xex directly
	// off the extracted directory rather than parsing XDVDFS bytes, so
	// there's no XDVDFS root to probe here.

	let iso: Uint8Array;
	let extractedParts: ReturnType<typeof convertXisoFixtureToExtractedParts>;

	beforeAll(() => {
		iso = makeFixture({ titleId: 0x6a6a0003, version: 2 });
		extractedParts = convertXisoFixtureToExtractedParts(iso);
	});

	it('parses without throwing', () => {
		expect(() =>
			inspectSource(nullReadFn, iso.length, {
				source: EXTRACTED_SOURCE,
				parts: extractedParts,
			}),
		).not.toThrow();
	});

	it('returns the same titleId/contentType as the original xiso fixture', () => {
		const original = inspectSource(makeReadFn(iso), iso.length, {
			source: { format: 'xiso' },
		});
		const extracted = inspectSource(nullReadFn, iso.length, {
			source: EXTRACTED_SOURCE,
			parts: extractedParts,
		});
		expect(extracted.titleId).toBe(original.titleId);
		expect(extracted.contentType).toBe(original.contentType);
		expect(extracted.version).toStrictEqual(original.version);
	});

	it('titleId is 8 uppercase hex digits', () => {
		const info = inspectSource(nullReadFn, iso.length, {
			source: EXTRACTED_SOURCE,
			parts: extractedParts,
		});
		expect(info.titleId).toMatch(/^[0-9A-F]{8}$/);
	});

	it('accepts a custom fetchSize without changing the result', () => {
		const info = inspectSource(nullReadFn, iso.length, {
			source: { ...EXTRACTED_SOURCE },
			parts: extractedParts,
		});
		expect(info.titleId).toBe('6A6A0003');
	});

	it('detects Xbox Original content type for an ogx-platform fixture', () => {
		const info = inspectSource(nullReadFn, iso.length, {
			source: EXTRACTED_SOURCE,
			parts: extractedParts,
		});
		expect(info.contentType).toBe('xboxOriginal');
	});

	it('detects Games on Demand content type for an x360-platform fixture', () => {
		const x360Iso = makeFixture({ titleId: 0x5a5a0003, platform: 'x360' });
		const parts = convertXisoFixtureToExtractedParts(x360Iso);
		const info = inspectSource(nullReadFn, x360Iso.length, {
			source: EXTRACTED_SOURCE,
			parts,
		});
		expect(info.titleId).toBe('5A5A0003');
		expect(info.contentType).toBe('gamesOnDemand');
	});

	it('parses correctly when other files are present alongside the launch executable', () => {
		const withUpdate = makeFixture({
			titleId: 0x41560001,
			includeSystemUpdate: true,
		});
		const parts = convertXisoFixtureToExtractedParts(withUpdate);
		expect(parts.length).toBeGreaterThanOrEqual(2);
		const info = inspectSource(nullReadFn, withUpdate.length, {
			source: EXTRACTED_SOURCE,
			parts,
		});
		expect(info.titleId).toBe('41560001');
	});

	it('still recognizes the launch executable when its name differs only by case', () => {
		const cases = ['DEFAULT.XBE', 'Default.Xbe', 'default.XBE', 'DeFaUlT.xBe'];
		for (const exeName of cases) {
			const withCasedName = makeFixture({ titleId: 0x6a6a0003, exeName });
			const parts = convertXisoFixtureToExtractedParts(withCasedName);
			const info = inspectSource(nullReadFn, withCasedName.length, {
				source: EXTRACTED_SOURCE,
				parts,
			});
			expect(info.titleId).toBe('6A6A0003');
			expect(info.contentType).toBe('xboxOriginal');
		}
	});

	it('still recognizes default.xex when its name differs only by case', () => {
		const cases = ['DEFAULT.XEX', 'Default.Xex', 'default.XEX', 'DeFaUlT.xEx'];
		for (const exeName of cases) {
			const withCasedName = makeFixture({
				titleId: 0x5a5a0003,
				platform: 'x360',
				exeName,
			});
			const parts = convertXisoFixtureToExtractedParts(withCasedName);
			const info = inspectSource(nullReadFn, withCasedName.length, {
				source: EXTRACTED_SOURCE,
				parts,
			});
			expect(info.titleId).toBe('5A5A0003');
			expect(info.contentType).toBe('gamesOnDemand');
		}
	});

	it('throws when no default.xbe/default.xex is present at root', () => {
		const bytes = new Uint8Array([1, 2, 3]);
		expect(() =>
			inspectSource(nullReadFn, bytes.length, {
				source: EXTRACTED_SOURCE,
				parts: [
					{ name: 'readme.txt', size: bytes.length, readFn: makeReadFn(bytes) },
				],
			}),
		).toThrow(/no default\.xbe\/default\.xex at root/);
	});

	it('throws for a default.xbe too small/malformed to contain a real header', () => {
		// Distinguishes "no executable found" (above) from "an executable
		// was found but its header couldn't be parsed".
		const bytes = new Uint8Array([1, 2, 3, 4]);
		expect(() =>
			inspectSource(nullReadFn, bytes.length, {
				source: EXTRACTED_SOURCE,
				parts: [
					{
						name: 'default.xbe',
						size: bytes.length,
						readFn: makeReadFn(bytes),
					},
				],
			}),
		).toThrow();
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			inspectSource(nullReadFn, iso.length, {
				source: EXTRACTED_SOURCE,
				parts: [],
			}),
		).toThrow();
	});

	it('propagates errors thrown inside a part readFn', () => {
		expect(() =>
			inspectSource(nullReadFn, 3, {
				source: EXTRACTED_SOURCE,
				parts: [{ name: 'default.xbe', size: 3, readFn: throwingReadFn }],
			}),
		).toThrow('read error from JS');
	});
});
