import { describe, it, expect, beforeAll, beforeEach, afterEach } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture, largeFileByteOffset } from '../../utils/fixtures/xsf.js';
import {
	makeReadFn,
	makeSparseReadFn,
	makeSpyReadFn,
	sawFetchCovering,
} from '../../utils/read-fns.js';
import { driveHashing, drain } from '../../utils/session-helpers.js';
import { ConversionSession, detectFormat } from '../../../dist/index.js';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

// outputName is required for ciso - split file names are derived from it
// ("<outputName>.1.cso", "<outputName>.2.cso", ...).
const OUTPUT_NAME = 'test';

describe('ConversionSession(ciso) two-pass sizing/streaming contract', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);
	let session: ConversionSession;

	beforeEach(() => {
		session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
	});

	afterEach(() => {
		session.free();
	});

	it('detectFormat resolves this fixture as xiso (the source shape ciso conversion consumes)', () => {
		expect(detectFormat(readFn, iso.length)).toBe('xiso');
	});

	it('opens without throwing', () => {
		// Session is opened in beforeEach; reaching this point means it didn't throw.
	});

	it('nextChunk() throws if called before hashNextPart() has finished sizing', () => {
		expect(() => session.nextChunk(1024)).toThrow();
	});

	it('hashNextPart() returns true once sizing is complete, and stays true on further calls', () => {
		driveHashing(session);
		expect(session.hashNextPart()).toBe(true);
		expect(session.hashNextPart()).toBe(true);
	});

	it('nextChunk() works once hashNextPart() has finished', () => {
		driveHashing(session);
		expect(() => session.nextChunk(1024)).not.toThrow();
	});
});

// Regression coverage for the same JsReader u64 -> usize truncation bug
// as conversion-session/xiso/large-source-boundary.test.ts
// (crate/src/core/reader.rs) - see that file's comments for the full
// rationale behind the anchors used below.
describe('ConversionSession(ciso) two-pass sizing at the 2^32-remaining-bytes boundary', () => {
	// Matches DEFAULT_BLOCK_SIZE in crate/src/core/reader.rs.
	const CACHE_BLOCK_SIZE = 64 * 1024;
	const TARGET_BLOCK_OFFSET = 2 * CACHE_BLOCK_SIZE; // 131072
	const LARGE_FILE_SIZE = 256 * 1024;

	// Passed to both makeFixture() and largeFileByteOffset() so the two
	// can't drift apart.
	const FIXTURE_OPTS = { titleId: 0x41560001, largeFileSize: LARGE_FILE_SIZE };
	const LARGE_FILE_BYTE_OFFSET = largeFileByteOffset(FIXTURE_OPTS)!; // 71680

	it('does not silently skip the fetch for the boundary block', () => {
		const iso = makeFixture(FIXTURE_OPTS);
		const size = TARGET_BLOCK_OFFSET + 2 ** 32;
		const { spy, calls } = makeSpyReadFn(makeSparseReadFn(iso));
		const session = ConversionSession.open(
			spy,
			size,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		expect(() => driveHashing(session)).not.toThrow();
		// Exhaust the streaming pass too - the bug could strike there
		// instead of during hashing.
		expect(() => drain(session, 1024 * 1024)).not.toThrow();
		// drain() already frees the session internally - don't free() again.
		expect(sawFetchCovering(calls, TARGET_BLOCK_OFFSET)).toBe(true);
	});

	it('sanity check: a size one byte off the boundary does not itself throw, and still fetches correctly', () => {
		const iso = makeFixture(FIXTURE_OPTS);
		const size = TARGET_BLOCK_OFFSET + 2 ** 32 - 1;
		const { spy, calls } = makeSpyReadFn(makeSparseReadFn(iso));
		const session = ConversionSession.open(
			spy,
			size,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		expect(() => driveHashing(session)).not.toThrow();
		expect(() => drain(session, 1024 * 1024)).not.toThrow();
		expect(sawFetchCovering(calls, TARGET_BLOCK_OFFSET)).toBe(true);
	});

	// Confirms the fetch check isn't specific to block 2 - see xiso file's
	// matching test for why an arbitrary offset wouldn't work instead.
	it('does not silently skip the fetch at a different block boundary (block 3), confirming this is not specific to block 2', () => {
		const ALT_BLOCK_OFFSET = 3 * CACHE_BLOCK_SIZE; // 196608
		const iso = makeFixture(FIXTURE_OPTS);
		const size = ALT_BLOCK_OFFSET + 2 ** 32;
		const { spy, calls } = makeSpyReadFn(makeSparseReadFn(iso));
		const session = ConversionSession.open(
			spy,
			size,
			{
				format: 'ciso',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
		expect(() => driveHashing(session)).not.toThrow();
		expect(() => drain(session, 1024 * 1024)).not.toThrow();
		expect(sawFetchCovering(calls, ALT_BLOCK_OFFSET)).toBe(true);
	});
});
