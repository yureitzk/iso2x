import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { inspectSource } from '../../dist/index.js';
import { makeReadFn, nullReadFn, throwingReadFn } from '../utils/read-fns.js';
import { convertXisoFixtureToBytes } from '../utils/session-helpers.js';
import { CISO_SOURCE } from '../utils/sources.js';

beforeAll(setupWasm);

describe('inspectSource with a `ciso` source', () => {
	let iso: Uint8Array;
	let cisoBytes: Uint8Array;

	beforeAll(() => {
		iso = makeFixture({ titleId: 0x41560001, version: 1 });
		cisoBytes = convertXisoFixtureToBytes(iso, {
			format: 'ciso',
			outputName: 'game',
		});
	});

	it('parses without throwing', () => {
		expect(() =>
			inspectSource(makeReadFn(cisoBytes), cisoBytes.length, CISO_SOURCE),
		).not.toThrow();
	});

	it('returns the same titleId/contentType as the original xiso fixture', () => {
		const original = inspectSource(makeReadFn(iso), iso.length, {
			source: { format: 'xiso' as const },
		});
		const converted = inspectSource(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			CISO_SOURCE,
		);
		expect(converted.titleId).toBe(original.titleId);
		expect(converted.contentType).toBe(original.contentType);
		expect(converted.version).toStrictEqual(original.version);
	});

	it('titleId is 8 uppercase hex digits', () => {
		const info = inspectSource(
			makeReadFn(cisoBytes),
			cisoBytes.length,
			CISO_SOURCE,
		);
		expect(info.titleId).toMatch(/^[0-9A-F]{8}$/);
	});

	it('accepts a custom fetchSize without changing the result', () => {
		const info = inspectSource(makeReadFn(cisoBytes), cisoBytes.length, {
			...CISO_SOURCE,
		});
		expect(info.titleId).toBe('41560001');
	});

	it('still throws for an invalid (zeroed) source declared as ciso', () => {
		expect(() =>
			inspectSource(nullReadFn, 10 * 1024 * 1024, CISO_SOURCE),
		).toThrow();
	});

	it('still propagates errors thrown inside the readFn', () => {
		expect(() =>
			inspectSource(throwingReadFn, 10 * 1024 * 1024, CISO_SOURCE),
		).toThrow('read error from JS');
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			inspectSource(makeReadFn(cisoBytes), cisoBytes.length, {
				source: CISO_SOURCE.source,
				parts: [],
			}),
		).toThrow();
	});
});
