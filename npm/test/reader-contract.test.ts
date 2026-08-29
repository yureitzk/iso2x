import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from './utils/wasm-setup.js';
import { makeFixture } from './utils/fixtures/xsf.js';
import { inspectSource } from '../dist/index.js';
import type { SourceRef } from '../dist/index.js';
import { makeReadFn, makeSparseReadFn } from './utils/read-fns.js';

beforeAll(setupWasm);

const XISO_SOURCE: SourceRef = { source: { format: 'xiso' } };

describe('readFn contract: misbehaving callback guards (reader.rs::call_read_fn)', () => {
	it('throws when readFn returns more bytes than requested', () => {
		const iso = makeFixture({ titleId: 0x52430001 });
		const real = makeReadFn(iso);
		// Always hands back one extra byte past what was asked for -
		// every call misbehaves, so this can't depend on which exact
		// offsets get fetched while parsing the fixture.
		const overReadFn = (offset: number, length: number): Uint8Array => {
			const extra = real(offset, length + 1);
			if (extra.length > length) return extra;
			// Near the physical end of the buffer there may not be a
			// real extra byte to hand back - pad with a zero so the
			// guard still sees more bytes than were requested.
			const padded = new Uint8Array(length + 1);
			padded.set(real(offset, length));
			return padded;
		};

		expect(() => inspectSource(overReadFn, iso.length, XISO_SOURCE)).toThrow(
			/readFn returned \d+ bytes but only \d+ were requested/,
		);
	});

	it('throws when readFn returns fewer bytes than requested and real data remains (not genuine EOF)', () => {
		const iso = makeFixture({ titleId: 0x52430002 });
		const real = makeReadFn(iso);
		// Chops one byte off every read. Since `iso.length` (the
		// declared size passed to inspectSource below) reflects the
		// real, complete fixture, any short read produced this way is
		// short of genuine EOF - exactly the "flaky fetch" case the
		// guard exists to catch rather than silently caching as EOF.
		const shortReadFn = (offset: number, length: number): Uint8Array =>
			real(offset, length).slice(0, Math.max(0, length - 1));

		expect(() => inspectSource(shortReadFn, iso.length, XISO_SOURCE)).toThrow(
			/treating as a failed fetch rather than EOF/,
		);
	});

	it('does not throw for a short read that genuinely reaches the declared end of the source', () => {
		// Contrast case for the guard above: makeSparseReadFn only
		// zero-pads past the *real* source length, and here the
		// declared `size` passed to inspectSource matches that real
		// length exactly, so any short read this produces is
		// legitimate EOF, not a failed fetch. This confirms the guard
		// is precise (fires on truncation, not on ordinary EOF-adjacent
		// reads) rather than merely present.
		const iso = makeFixture({ titleId: 0x52430003 });
		expect(() =>
			inspectSource(makeSparseReadFn(iso), iso.length, XISO_SOURCE),
		).not.toThrow();
	});

	it('throws when readFn returns something that is not a Uint8Array', () => {
		const iso = makeFixture({ titleId: 0x52430004 });
		const nonUint8ArrayReadFn = (_offset: number, length: number): number[] =>
			Array.from({ length }, () => 0);

		expect(() =>
			inspectSource(
				// @ts-expect-error - intentionally violating SourceReadFn's
				// return type to assert the runtime contract for callers
				// not using TS (or misimplementing it despite TS).
				nonUint8ArrayReadFn,
				iso.length,
				XISO_SOURCE,
			),
		).toThrow(/read_fn did not return Uint8Array/);
	});

	it('propagates a JS Error whose message contains the formatted Rust error text (not a generic/opaque failure)', () => {
		// Reader.rs formats these as `io::Error::other(...)` on the Rust
		// side; this pins down that the wasm boundary carries the
		// actual message through to a real JS Error rather than
		// collapsing it into something generic like "RuntimeError" or
		// a bare abort.
		const iso = makeFixture({ titleId: 0x52430005 });
		const real = makeReadFn(iso);
		const overReadFn = (offset: number, length: number): Uint8Array => {
			const padded = new Uint8Array(length + 1);
			padded.set(real(offset, length));
			return padded;
		};

		let caught: unknown;
		try {
			inspectSource(overReadFn, iso.length, XISO_SOURCE);
		} catch (e) {
			caught = e;
		}
		expect(caught).toBeInstanceOf(Error);
		expect((caught as Error).message).toMatch(/were requested/);
	});
});
