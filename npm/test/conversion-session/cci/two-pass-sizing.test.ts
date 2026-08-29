import { describe, it, expect, beforeAll, beforeEach, afterEach } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { makeReadFn } from '../../utils/read-fns.js';
import { driveHashing } from '../../utils/session-helpers.js';
import { ConversionSession, detectFormat } from '../../../dist/index.js';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

// outputName is required for cci - split file names are derived from it
// ("<outputName>.1.cci", "<outputName>.2.cci", ...).
const OUTPUT_NAME = 'test';

describe('ConversionSession(cci) two-pass sizing/streaming contract', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);
	let session: ConversionSession;

	beforeEach(() => {
		session = ConversionSession.open(
			readFn,
			iso.length,
			{
				format: 'cci',
				outputName: OUTPUT_NAME,
			},
			XISO_SOURCE,
		);
	});

	afterEach(() => {
		session.free();
	});

	it('detectFormat resolves this fixture as xiso (the source shape cci conversion consumes)', () => {
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
