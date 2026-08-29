import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture, largeFileByteOffset } from '../../utils/fixtures/xsf.js';
import { ConversionSession, SourceRef } from '../../../dist/index.js';
import {
	makeSparseReadFn,
	makeSpyReadFn,
	sawFetchCovering,
} from '../../utils/read-fns.js';
import { drain } from '../../utils/session-helpers.js';

beforeAll(setupWasm);

const XISO_SOURCE: SourceRef = { source: { format: 'xiso' } };

// Matches DEFAULT_BLOCK_SIZE in crate/src/core/reader.rs.
const CACHE_BLOCK_SIZE = 64 * 1024;

// LARGE_FILE_BYTE_OFFSET (block 1) gets pre-warmed during structural
// parsing, masking the bug there (verified: 6/6 passed on broken code
// too). Block 2 is confirmed cold, so anchor here instead.
const TARGET_BLOCK_OFFSET = 2 * CACHE_BLOCK_SIZE; // 131072

// Spans block 1 (content start) through block 2 (TARGET_BLOCK_OFFSET)
// into block 3. Don't shrink without re-verifying against broken
// reader.rs that the block 2 fetch stays uncovered by pre-warming.
const LARGE_FILE_SIZE = 256 * 1024;

// Passed to both makeFixture() and largeFileByteOffset() so the two
// can't drift apart.
const FIXTURE_OPTS = { titleId: 0x41560001, largeFileSize: LARGE_FILE_SIZE };
const LARGE_FILE_BYTE_OFFSET = largeFileByteOffset(FIXTURE_OPTS)!; // 71680

describe('ConversionSession(xiso, mode=full) at the 2^32-remaining-bytes boundary', () => {
	it('does not silently skip the fetch for a fresh, never-yet-touched block at the boundary', () => {
		const iso = makeFixture(FIXTURE_OPTS);
		const size = TARGET_BLOCK_OFFSET + 2 ** 32;
		const { spy, calls } = makeSpyReadFn(makeSparseReadFn(iso));
		const session = ConversionSession.open(
			spy,
			size,
			{ format: 'xiso', mode: 'full' },
			XISO_SOURCE,
		);
		expect(() => drain(session, 1024 * 1024)).not.toThrow();
		// drain() already frees the session internally - don't free() again.
		expect(sawFetchCovering(calls, TARGET_BLOCK_OFFSET)).toBe(true);
	});

	// Not-throwing alone isn't enough: a truncating cast only throws when
	// the truncated value happens to be too small, so also assert the
	// fetch actually happened.
	it('sanity check: a size one byte off the boundary does not itself throw, and still fetches correctly', () => {
		const iso = makeFixture(FIXTURE_OPTS);
		const size = TARGET_BLOCK_OFFSET + 2 ** 32 - 1;
		const { spy, calls } = makeSpyReadFn(makeSparseReadFn(iso));
		const session = ConversionSession.open(
			spy,
			size,
			{ format: 'xiso', mode: 'full' },
			XISO_SOURCE,
		);
		expect(() => drain(session, 1024 * 1024)).not.toThrow();
		expect(sawFetchCovering(calls, TARGET_BLOCK_OFFSET)).toBe(true);
	});

	// Sweep multiples so a fix that only special-cases exactly 2^32
	// remaining doesn't slip through.
	it.each([2, 3, 5])(
		'does not silently skip the fetch at %d * 2^32 remaining bytes either',
		(multiple) => {
			const iso = makeFixture(FIXTURE_OPTS);
			const size = TARGET_BLOCK_OFFSET + multiple * 2 ** 32;
			const { spy, calls } = makeSpyReadFn(makeSparseReadFn(iso));
			const session = ConversionSession.open(
				spy,
				size,
				{ format: 'xiso', mode: 'full' },
				XISO_SOURCE,
			);
			expect(() => drain(session, 1024 * 1024)).not.toThrow();
			expect(sawFetchCovering(calls, TARGET_BLOCK_OFFSET)).toBe(true);
		},
	);

	// An arbitrary non-aligned offset doesn't add coverage: the truncation
	// only zeroes `want` at one exact position, and whole-block fetches
	// mask anywhere else in that block. Coverage requires anchoring on a
	// fresh block boundary - this one confirms it's not specific to block 2.
	it('does not silently skip the fetch at a different block boundary (block 3), confirming this is not specific to block 2', () => {
		const ALT_BLOCK_OFFSET = 3 * CACHE_BLOCK_SIZE; // 196608
		const iso = makeFixture(FIXTURE_OPTS);
		const size = ALT_BLOCK_OFFSET + 2 ** 32;
		const { spy, calls } = makeSpyReadFn(makeSparseReadFn(iso));
		const session = ConversionSession.open(
			spy,
			size,
			{ format: 'xiso', mode: 'full' },
			XISO_SOURCE,
		);
		expect(() => drain(session, 1024 * 1024)).not.toThrow();
		expect(sawFetchCovering(calls, ALT_BLOCK_OFFSET)).toBe(true);
	});

	it('does not stop partway through reading the boundary file', () => {
		const iso = makeFixture(FIXTURE_OPTS);
		const size = TARGET_BLOCK_OFFSET + 2 ** 32;
		const { spy, calls } = makeSpyReadFn(makeSparseReadFn(iso));
		const session = ConversionSession.open(
			spy,
			size,
			{ format: 'xiso', mode: 'full' },
			XISO_SOURCE,
		);
		expect(() => drain(session, 1024 * 1024)).not.toThrow();
		const maxPositionReached = calls.reduce(
			(max, c) => Math.max(max, c.offset + c.length),
			0,
		);
		expect(maxPositionReached).toBeGreaterThanOrEqual(
			LARGE_FILE_BYTE_OFFSET + LARGE_FILE_SIZE,
		);
	});
});
