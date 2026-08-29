import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { ConversionSession } from '../../dist/index.js';
import type { SourceRef } from '../../dist/index.js';
import { makeReadFn } from '../utils/read-fns.js';

beforeAll(async () => {
	await setupWasm();
});

const XISO_SOURCE: SourceRef = { source: { format: 'xiso' } };

describe('ConversionSession implements [Symbol.dispose] for `using`', () => {
	it('using disposes the session automatically at the end of its block', () => {
		const iso = makeFixture({ titleId: 0x44495001 });
		let sessionRef: ConversionSession;
		{
			using session = ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'xiso' },
				XISO_SOURCE,
			);
			sessionRef = session;
			session.nextChunk(100);
		}
		// The block above exited without an explicit free() call - if
		// [Symbol.dispose] wasn't forwarded to the raw session, this
		// would still succeed (leaking the handle) instead of throwing.
		expect(() => sessionRef.isDone()).toThrow();
	});

	it('disposing via `using` is equivalent to calling free() explicitly - a further call throws either way', () => {
		const iso = makeFixture({ titleId: 0x44495002 });

		const explicit = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'xiso' },
			XISO_SOURCE,
		);
		explicit.free();
		let explicitError: unknown;
		try {
			explicit.isDone();
		} catch (e) {
			explicitError = e;
		}

		let viaUsing: ConversionSession;
		{
			using session = ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'xiso' },
				XISO_SOURCE,
			);
			viaUsing = session;
		}
		let usingError: unknown;
		try {
			viaUsing.isDone();
		} catch (e) {
			usingError = e;
		}

		expect(explicitError).toBeInstanceOf(Error);
		expect(usingError).toBeInstanceOf(Error);
	});
});
