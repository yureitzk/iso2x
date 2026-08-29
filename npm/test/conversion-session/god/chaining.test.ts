import { describe, it, expect, beforeAll, vi } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { ConversionSession, mhtSize } from '../../../dist/index.js';
import {
	driveHashing,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import { makeSparseReadFn } from '../../utils/read-fns.js';
import * as crypto from 'crypto';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

function computeSha1(buffer: Uint8Array): Uint8Array {
	return crypto.createHash('sha1').update(buffer).digest();
}

/**
 * Shared assertion used by the backward-chaining tests below: given a
 * finished session, drains it while collecting each part's leading MHT
 * header (identified by exact mhtSize() length within a `.data/Data`
 * entry), then verifies that every part's embedded hash - 36 bytes
 * before the end of its MHT, 20 bytes long - matches the real SHA-1 of
 * the *next* part's MHT bytes. Also asserts there's more than one part
 * (otherwise the chain has nothing to verify) and that no embedded hash
 * is left all-zero (i.e. actually computed, not just allocated).
 */
function assertBackwardChaining(session: ConversionSession): void {
	const MHT_SIZE = mhtSize();
	const mhtHeaders: Uint8Array[] = [];
	while (!session.isDone()) {
		const chunk = session.nextChunk(1024 * 1024);
		if (!chunk) continue;
		const currentEntry = session.currentEntryName();
		if (
			currentEntry &&
			currentEntry.includes('.data/Data') &&
			chunk.length === MHT_SIZE
		) {
			mhtHeaders.push(new Uint8Array(chunk));
		}
	}
	expect(mhtHeaders.length).toBeGreaterThan(1);
	for (let i = 0; i < mhtHeaders.length - 1; i++) {
		const currentMht = mhtHeaders[i];
		const nextMht = mhtHeaders[i + 1];
		const expectedNextHash = computeSha1(nextMht);
		const embeddedHashOffset = MHT_SIZE - 36;
		const embeddedHash = currentMht.slice(
			embeddedHashOffset,
			embeddedHashOffset + 20,
		);
		const isAllZeroes = Array.from(embeddedHash).every((byte) => byte === 0);
		expect(isAllZeroes).toBe(false);
		const embeddedHex = Buffer.from(embeddedHash).toString('hex');
		const expectedHex = Buffer.from(expectedNextHash).toString('hex');
		expect(embeddedHex).toBe(expectedHex);
	}
}

describe("ConversionSession(god) header MHT chaining reflects each part's master MHT", () => {
	const largeFileSize = 4 * 1024 * 1024;
	const iso = makeFixture({ titleId: 0x41560001, largeFileSize });
	const fileSize = iso.length + largeFileSize;

	it('changing ISO content changes the final header bytes', () => {
		const readFn1 = makeSparseReadFn(iso);
		const session1 = ConversionSession.open(
			readFn1,
			fileSize,
			{
				format: 'god',
			},
			XISO_SOURCE,
		);
		driveHashing(session1);
		const chunks1: Uint8Array[] = [];
		while (!session1.isDone()) {
			const c = session1.nextChunk(UNBOUNDED_CHUNK_SIZE);
			if (c) chunks1.push(c);
		}
		session1.free();
		const header1 = chunks1[chunks1.length - 1];
		const mutated = iso.slice();
		mutated[mutated.length - 1] ^= 0xff;
		const readFn2 = makeSparseReadFn(mutated);
		const session2 = ConversionSession.open(
			readFn2,
			fileSize,
			{
				format: 'god',
			},
			XISO_SOURCE,
		);
		driveHashing(session2);
		const chunks2: Uint8Array[] = [];
		while (!session2.isDone()) {
			const c = session2.nextChunk(UNBOUNDED_CHUNK_SIZE);
			if (c) chunks2.push(c);
		}
		session2.free();
		const header2 = chunks2[chunks2.length - 1];
		expect(header1).not.toEqual(header2);
	});
});

describe('GodSession Backward Cryptographic Chaining', () => {
	// All three tests below share the same shape: build a multi-part image
	// (large enough that GoD splits it into more than one part for every
	// mode), drive it to completion, then verify - via
	// assertBackwardChaining() - that each part's MHT embeds the real
	// SHA-1 hash of the *next* part's MHT bytes, 36 bytes before the end
	// of the (fixed-size) MHT structure. This is exercised separately for
	// each ScrubMode because 'none'/'partial' use the Direct backend
	// (straight sector copy from the source) while 'full' uses the
	// Rebuild backend (fresh XDVDFS reauthor) - they are independent code
	// paths and a chaining bug in one would not necessarily show up in
	// the other.
	//
	// None of these three care what sequentialWindow resolves to - only
	// the MHT hash chain is asserted on - so they all just take
	// ConversionSession.open()'s default (omitted sequentialWindow ->
	// core::reader::DEFAULT_SEQ_WINDOW).
	it('should correctly embed the SHA-1 hash of Part N+1 into Part N near the end of the MHT', () => {
		const multiPartFileSize = Math.floor(1.2 * 1024 * 1024 * 1024);
		const iso = makeFixture({ titleId: 0x41560001 });
		const largeReadFn = makeSparseReadFn(iso);
		const session = ConversionSession.open(
			largeReadFn,
			multiPartFileSize,
			{
				format: 'god',
				mode: 'none',
				gameTitle: 'Test Game',
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		assertBackwardChaining(session);
		session.free();
	}, 30000);

	it('should also chain correctly under mode: "partial" (Direct backend, trim + zero)', () => {
		// Unlike 'none', 'partial' trims everything past the last *used*
		// byte before streaming - a mostly-zero container (as used for the
		// 'none' case above) would be trimmed down to almost nothing,
		// collapsing this to a single part with no chain to verify. Give
		// it a large *declared* file extent (same trick the 'full' test
		// uses) so there's real content for partial's trim pass to keep.
		const multiPartFileSize = Math.floor(1.2 * 1024 * 1024 * 1024);
		const iso = makeFixture({
			titleId: 0x41560001,
			largeFileSize: 1.1 * 1024 * 1024 * 1024,
		});
		const largeReadFn = makeSparseReadFn(iso);
		const session = ConversionSession.open(
			largeReadFn,
			multiPartFileSize,
			{
				format: 'god',
				mode: 'partial',
				gameTitle: 'Test Game',
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		assertBackwardChaining(session);
		session.free();
	}, 30000);

	it('should also chain correctly under mode: "full" (rebuilt XDVDFS backend)', () => {
		const multiPartFileSize = Math.floor(1.2 * 1024 * 1024 * 1024);
		const iso = makeFixture({
			titleId: 0x41560001,
			largeFileSize: 1.1 * 1024 * 1024 * 1024,
		});
		const largeReadFn = makeSparseReadFn(iso);
		const session = ConversionSession.open(
			largeReadFn,
			multiPartFileSize,
			{
				format: 'god',
				mode: 'full',
				gameTitle: 'Test Game',
			},
			XISO_SOURCE,
		);
		driveHashing(session);
		assertBackwardChaining(session);
		session.free();
	}, 30000);
});

// Regression test: GOD conversion should use JsReader's big-window
// Sequential mode, not small-block Cached mode (which caused a bulk
// conversion slowdown - see handoff). Checked via readFn call count/size
// instead of wall-clock timing, since timing alone is noisy.
// Confirmed fix: 549 calls (cached) -> 309-310 calls (sequential).
describe('GOD conversion (mode: "none") drives readFn through Sequential mode, not Cached mode', () => {
	// JsReader's Sequential window size (crate/src/core/reader.rs). Not
	// exposed via wasm, so duplicated here - update if that constant changes.
	const DEFAULT_SEQ_WINDOW = 8 * 1024 * 1024;
	// Old Cached-mode baseline: 549 calls. Fixed (Sequential mode) observed
	// run: 309-310 calls. Bound set with headroom above the observed count.
	const MAX_CALL_COUNT = 400;

	it('reads via large sequential windows instead of many small cached blocks', () => {
		const multiPartFileSize = Math.floor(1.2 * 1024 * 1024 * 1024);
		const iso = makeFixture({ titleId: 0x41560001 });
		const readFnSpy = vi.fn(makeSparseReadFn(iso));
		const session = ConversionSession.open(
			readFnSpy,
			multiPartFileSize,
			{
				format: 'god',
				mode: 'none',
				gameTitle: 'Test Game',
			},
			XISO_SOURCE,
			// sourceParts, sequentialWindow both omitted - this must use
			// ConversionSession.open()'s real default window
			// (core::reader::DEFAULT_SEQ_WINDOW, 8 MiB), not a small one:
			// that's the whole thing this test exists to confirm.
		);
		driveHashing(session);
		while (!session.isDone()) {
			session.nextChunk(1024 * 1024);
		}

		session.free();
		expect(readFnSpy.mock.calls.length).toBeGreaterThan(0);
		expect(readFnSpy.mock.calls.length).toBeLessThan(MAX_CALL_COUNT);
		// Confirms it's actually Sequential mode's big window, not just
		// fewer calls for some unrelated reason.
		const requestedLengths = readFnSpy.mock.calls.map(
			(call) => call[1] as number,
		);
		const sawSequentialWindow = requestedLengths.some(
			(len) => len >= DEFAULT_SEQ_WINDOW / 2,
		);
		expect(sawSequentialWindow).toBe(true);
	}, 30000);

	// Inverse of the test above: confirms sequentialWindow is actually
	// load-bearing, not just present in the signature. A small window
	// forces many more, smaller round trips than the default - this is
	// the mirror image of "large window, few calls" and catches a
	// regression where the parameter stops reaching the reader (e.g. if
	// it silently fell back to the default instead of being threaded
	// through core::source::open).
	it('a small sequentialWindow increases call count relative to the default', () => {
		const multiPartFileSize = Math.floor(1.2 * 1024 * 1024 * 1024);
		const iso = makeFixture({ titleId: 0x41560009 });
		const readFnSpy = vi.fn(makeSparseReadFn(iso));
		const session = ConversionSession.open(
			readFnSpy,
			multiPartFileSize,
			{
				format: 'god',
				mode: 'none',
				gameTitle: 'Test Game',
			},
			XISO_SOURCE,
			64 * 1024,
		);
		driveHashing(session);
		while (!session.isDone()) {
			session.nextChunk(1024 * 1024);
		}

		session.free();
		// Well above MAX_CALL_COUNT (400) - a ~1.2 GiB pass at a 64 KiB
		// window takes tens of thousands of round trips, vs. ~309-310 at
		// the real 8 MiB default.
		expect(readFnSpy.mock.calls.length).toBeGreaterThan(1000);
	}, 30000);
});
