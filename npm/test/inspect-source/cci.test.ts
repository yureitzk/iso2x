import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { inspectSource } from '../../dist/index.js';
import { makeReadFn, nullReadFn, throwingReadFn } from '../utils/read-fns.js';
import { convertXisoFixtureToBytes } from '../utils/session-helpers.js';
import { CCI_SOURCE } from '../utils/sources.js';

beforeAll(setupWasm);

describe('inspectSource with a `cci` source', () => {
	let iso: Uint8Array;
	let cciBytes: Uint8Array;

	beforeAll(() => {
		iso = makeFixture({ titleId: 0x7a7a0002, version: 3 });
		cciBytes = convertXisoFixtureToBytes(iso, {
			format: 'cci',
			outputName: 'game',
		});
	});

	it('parses without throwing', () => {
		expect(() =>
			inspectSource(makeReadFn(cciBytes), cciBytes.length, CCI_SOURCE),
		).not.toThrow();
	});

	it('returns the same titleId/contentType/version as the original xiso fixture', () => {
		const original = inspectSource(makeReadFn(iso), iso.length, {
			source: { format: 'xiso' as const },
		});
		const converted = inspectSource(
			makeReadFn(cciBytes),
			cciBytes.length,
			CCI_SOURCE,
		);
		expect(converted.titleId).toBe(original.titleId);
		expect(converted.contentType).toBe(original.contentType);
		expect(converted.version).toStrictEqual(original.version);
	});

	it('titleId is 8 uppercase hex digits', () => {
		const info = inspectSource(makeReadFn(cciBytes), cciBytes.length, CCI_SOURCE);
		expect(info.titleId).toMatch(/^[0-9A-F]{8}$/);
	});

	it('accepts a custom fetchSize without changing the result', () => {
		const info = inspectSource(makeReadFn(cciBytes), cciBytes.length, {
			...CCI_SOURCE,
		});
		expect(info.titleId).toBe('7A7A0002');
	});

	it('still throws for an invalid (zeroed) source declared as cci', () => {
		expect(() =>
			inspectSource(nullReadFn, 10 * 1024 * 1024, CCI_SOURCE),
		).toThrow();
	});

	it('still propagates errors thrown inside the readFn', () => {
		expect(() =>
			inspectSource(throwingReadFn, 10 * 1024 * 1024, CCI_SOURCE),
		).toThrow('read error from JS');
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			inspectSource(makeReadFn(cciBytes), cciBytes.length, {
				source: CCI_SOURCE.source,
				parts: [],
			}),
		).toThrow();
	});
});
