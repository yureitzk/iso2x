import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { detectFormat, inspectSource } from '../../dist/index.js';
import { makeReadFn, nullReadFn, throwingReadFn } from '../utils/read-fns.js';
import { convertXisoFixtureToBytes } from '../utils/session-helpers.js';
import type { SourceRef } from '../../dist/types.js';

beforeAll(setupWasm);

const ZAR_SOURCE: SourceRef = { source: { format: 'zar' } };

describe('inspectSource with a `zar` source', () => {
	let iso: Uint8Array;
	let zarBytes: Uint8Array;

	beforeAll(() => {
		iso = makeFixture({ titleId: 0x6a6a0003, version: 2 });
		zarBytes = convertXisoFixtureToBytes(iso, {
			format: 'zar',
			outputName: 'game',
		});
	});

	it('detectFormat resolves the converted bytes as zar (footer-magic detection)', () => {
		expect(detectFormat(makeReadFn(zarBytes), zarBytes.length)).toBe('zar');
	});

	it('parses without throwing', () => {
		expect(() =>
			inspectSource(makeReadFn(zarBytes), zarBytes.length, ZAR_SOURCE),
		).not.toThrow();
	});

	it('returns the same titleId/contentType as the original xiso fixture', () => {
		const original = inspectSource(makeReadFn(iso), iso.length, {
			source: { format: 'xiso' },
		});
		const converted = inspectSource(
			makeReadFn(zarBytes),
			zarBytes.length,
			ZAR_SOURCE,
		);
		expect(converted.titleId).toBe(original.titleId);
		expect(converted.contentType).toBe(original.contentType);
		expect(converted.version).toStrictEqual(original.version);
	});

	it('titleId is 8 uppercase hex digits', () => {
		const info = inspectSource(makeReadFn(zarBytes), zarBytes.length, ZAR_SOURCE);
		expect(info.titleId).toMatch(/^[0-9A-F]{8}$/);
	});

	it('accepts a custom fetchSize without changing the result', () => {
		const info = inspectSource(makeReadFn(zarBytes), zarBytes.length, {
			...ZAR_SOURCE,
		});
		expect(info.titleId).toBe('6A6A0003');
	});

	it('detects Games on Demand content type for an x360-platform fixture converted to zar', () => {
		const x360Iso = makeFixture({ titleId: 0x5a5a0003, platform: 'x360' });
		const x360Zar = convertXisoFixtureToBytes(x360Iso, {
			format: 'zar',
			outputName: 'game',
		});
		const info = inspectSource(makeReadFn(x360Zar), x360Zar.length, ZAR_SOURCE);
		expect(info.titleId).toBe('5A5A0003');
		expect(info.contentType).toBe('gamesOnDemand');
	});

	it('still throws for an invalid (zeroed) source declared as zar', () => {
		expect(() =>
			inspectSource(nullReadFn, zarBytes.length, ZAR_SOURCE),
		).toThrow();
	});

	it('still propagates errors thrown inside readFn', () => {
		expect(() =>
			inspectSource(throwingReadFn, zarBytes.length, ZAR_SOURCE),
		).toThrow('read error from JS');
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			inspectSource(nullReadFn, zarBytes.length, {
				source: { format: 'zar' },
				parts: [],
			}),
		).toThrow();
	});
});
