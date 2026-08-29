import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { inspectSource } from '../../dist/index.js';
import { makeReadFn, nullReadFn, throwingReadFn } from '../utils/read-fns.js';
import type { SourceRef } from '../../dist/types.js';

beforeAll(setupWasm);

const XISO_SOURCE: SourceRef = { source: { format: 'xiso' } };

describe('inspectSource with a `xiso` source: error paths', () => {
	it('throws a JS Error for an invalid (zeroed) image', () => {
		expect(() =>
			inspectSource(nullReadFn, 10 * 1024 * 1024, XISO_SOURCE),
		).toThrow();
	});

	it('propagates errors thrown inside the readFn', () => {
		expect(() =>
			inspectSource(throwingReadFn, 10 * 1024 * 1024, XISO_SOURCE),
		).toThrow('read error from JS');
	});

	it('throws for a zero file size', () => {
		expect(() => inspectSource(nullReadFn, 0, XISO_SOURCE)).toThrow();
	});

	// `source` is required - omitting it must fail loudly rather than
	// silently assuming xiso, so nothing can skip the resolve step.
	it('throws when source is omitted, instead of silently assuming xiso', () => {
		expect(() =>
			// @ts-expect-error - source is intentionally omitted to assert
			// the runtime contract for callers not using TS.
			inspectSource(nullReadFn, 10 * 1024 * 1024),
		).toThrow(/source format must be resolved/);
	});
});

describe('inspectSource with a `xiso` source: minimal XBE fixture', () => {
	let iso: Uint8Array;
	let readFn: ReturnType<typeof makeReadFn>;

	beforeAll(() => {
		iso = makeFixture({ titleId: 0x41560001, version: 1 });
		readFn = makeReadFn(iso);
	});

	it('parses without throwing', () => {
		expect(() => inspectSource(readFn, iso.length, XISO_SOURCE)).not.toThrow();
	});

	it('returns correct titleId', () => {
		const info = inspectSource(readFn, iso.length, XISO_SOURCE);
		expect(info.titleId).toBe('41560001');
	});

	it('detects XboxOriginal content type (XBE = original Xbox game)', () => {
		const info = inspectSource(readFn, iso.length, XISO_SOURCE);
		expect(info.contentType).toBe('xboxOriginal');
	});

	it('titleId is 8 uppercase hex digits', () => {
		const info = inspectSource(readFn, iso.length, XISO_SOURCE);
		expect(info.titleId).toMatch(/^[0-9A-F]{8}$/);
	});

	it('returns different titleIds for different fixtures', () => {
		const iso2 = makeFixture({ titleId: 0xdeadbeef });
		const info1 = inspectSource(readFn, iso.length, XISO_SOURCE);
		const info2 = inspectSource(makeReadFn(iso2), iso2.length, XISO_SOURCE);
		expect(info1.titleId).not.toBe(info2.titleId);
	});

	// `makeFixture({ platform: 'x360' })` writes a real minimal XEX2-shaped
	// stub (magic + one ExecutionId header field + a packed
	// TitleExecutionInfo) - see utils/fixtures/xsf.ts's `writeXexStub` for
	// the exact byte layout. This is a different code path from
	// filename-only platform detection: here the executable's *content* is
	// actually parsed, so a fixture that only renamed the directory entry
	// to "default.xex" without writing valid XEX2 bytes would (correctly)
	// throw "missing 'XEX2' magic bytes in XEX header".
	it('detects Games on Demand content type via a real XEX2 stub (default.xex)', () => {
		const x360Iso = makeFixture({ titleId: 0x5a5a0001, platform: 'x360' });
		const info = inspectSource(makeReadFn(x360Iso), x360Iso.length, XISO_SOURCE);
		expect(info.titleId).toBe('5A5A0001');
		expect(info.contentType).toBe('gamesOnDemand');
	});
});

describe('inspectSource with a `xiso` source: explicit source option', () => {
	let iso: Uint8Array;
	let readFn: ReturnType<typeof makeReadFn>;

	beforeAll(() => {
		iso = makeFixture({ titleId: 0x41560001, version: 1 });
		readFn = makeReadFn(iso);
	});

	it('produces identical results across repeated explicit-source calls', () => {
		const first = inspectSource(readFn, iso.length, XISO_SOURCE);
		const second = inspectSource(readFn, iso.length, XISO_SOURCE);
		expect(second).toEqual(first);
	});

	// Regression test for a multi-part reader bug: seek logic compared a
	// buffered-window position (end of the currently *buffered* window,
	// not the logical read cursor) against the target offset to decide
	// whether a reseek was needed. At small fetchSize values, a read can
	// legitimately land exactly on that buffer-end boundary while stale
	// unread bytes are still sitting in the buffer, so the reseek got
	// skipped and the wrong bytes were served - reproducibly, for this
	// exact fixture size, at fetchSize: 4096. Fixed by always delegating
	// to the single-part reader's seek logic (which already does the
	// correct buffered-window check) instead of re-deciding it for the
	// multi-part case.
	it('accepts a custom fetchSize without changing the result', () => {
		const info = inspectSource(readFn, iso.length, {
			...XISO_SOURCE,
		});
		expect(info.titleId).toBe('41560001');
	});

	it('still throws for an invalid (zeroed) image', () => {
		expect(() =>
			inspectSource(nullReadFn, 10 * 1024 * 1024, XISO_SOURCE),
		).toThrow();
	});

	it('still propagates errors thrown inside the readFn', () => {
		expect(() =>
			inspectSource(throwingReadFn, 10 * 1024 * 1024, XISO_SOURCE),
		).toThrow('read error from JS');
	});
});

describe('inspectSource with a `xiso` source: multi-part sourceParts', () => {
	let iso: Uint8Array;
	let sourceParts: {
		name: string;
		size: number;
		readFn: ReturnType<typeof makeReadFn>;
	}[];

	beforeAll(() => {
		iso = makeFixture({ titleId: 0x41560001, version: 1 });
		const splitPoint = Math.floor(iso.length / 2);
		const part1 = iso.slice(0, splitPoint);
		const part2 = iso.slice(splitPoint);
		sourceParts = [
			{ name: 'game.1.iso', size: part1.length, readFn: makeReadFn(part1) },
			{ name: 'game.2.iso', size: part2.length, readFn: makeReadFn(part2) },
		];
	});

	it('parses a source split across two parts as one contiguous stream', () => {
		// readFn/fileSize are unused for the multi-part case (they're only
		// used as a fallback when parts is undefined/null), but
		// inspectSource still requires them positionally.
		const info = inspectSource(nullReadFn, iso.length, {
			source: { format: 'xiso' },
			parts: sourceParts,
		});
		expect(info.titleId).toBe('41560001');
	});

	it('matches the single-part result for the same underlying image', () => {
		const singlePart = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE);
		const multiPart = inspectSource(nullReadFn, iso.length, {
			source: { format: 'xiso' },
			parts: sourceParts,
		});
		expect(multiPart).toEqual(singlePart);
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			inspectSource(nullReadFn, iso.length, {
				source: { format: 'xiso' },
				parts: [],
			}),
		).toThrow();
	});
});

describe('inspectSource surfaces `version` in SourceInfo', () => {
	it('reports a flat build number for an XBE version', () => {
		const iso = makeFixture({ titleId: 0x41560001, version: 4114 });
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE);
		expect(info.version).toStrictEqual({ kind: 'xbe', build: 4114 });
	});

	it('reports the packed fields for an XEX version', () => {
		// (major=2, minor=1, build=12345, qfe=6) packed per the free60/
		// idaxex layout: major:4, minor:4, build:16, qfe:8, MSB to LSB.
		const iso = makeFixture({
			titleId: 0x5a5a0002,
			version: 0x21303906,
			platform: 'x360',
		});
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE);
		expect(info.version).toStrictEqual({
			kind: 'xex',
			version: { major: 2, minor: 1, build: 12345, qfe: 6 },
			base: undefined,
		});
	});

	it('omits `base` when base_version is zero (not a patch)', () => {
		const iso = makeFixture({
			titleId: 0x5a5a0003,
			version: 0x21303906,
			platform: 'x360',
			// baseVersion omitted -> defaults to 0
		});
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE);
		expect(info.version).toMatchObject({ base: undefined });
	});

	it('reports `base` when base_version is nonzero (a patch title)', () => {
		// (major=1, minor=0, build=100, qfe=0) packed the same way.
		const iso = makeFixture({
			titleId: 0x5a5a0004,
			version: 0x21303906,
			baseVersion: 0x10006400,
			platform: 'x360',
		});
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE);
		expect(info.version).toStrictEqual({
			kind: 'xex',
			version: { major: 2, minor: 1, build: 12345, qfe: 6 },
			base: { major: 1, minor: 0, build: 100, qfe: 0 },
		});
	});

	it('reports build 0 for an OGX title with version 0, without packed decoding', () => {
		const iso = makeFixture({
			titleId: 0x5a5a000a,
			platform: 'ogx',
			version: 0,
		});
		const info = inspectSource(makeReadFn(iso), iso.length, {
			source: { format: 'xiso' },
		});
		expect(info.version).toStrictEqual({ kind: 'xbe', build: 0 });
	});
});
