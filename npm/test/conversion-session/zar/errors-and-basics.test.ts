import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import {
	DEFAULT_XBE_DECLARED_SIZE,
	makeFixture,
} from '../../utils/fixtures/xsf.js';
import { ConversionSession, detectFormat } from '../../../dist/index.js';
import {
	makeReadFn,
	nullReadFn,
	throwingReadFn,
} from '../../utils/read-fns.js';
import { UNBOUNDED_CHUNK_SIZE } from '../../utils/session-helpers.js';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

const OUTPUT_NAME = 'game';

describe('ConversionSession(zar) error paths', () => {
	it('throws for a zeroed (invalid) image', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				10 * 1024 * 1024,
				{ format: 'zar', outputName: OUTPUT_NAME },
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('propagates errors thrown inside readFn', () => {
		expect(() =>
			ConversionSession.open(
				throwingReadFn,
				10 * 1024 * 1024,
				{ format: 'zar', outputName: OUTPUT_NAME },
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('throws for a zero file size', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'zar', outputName: OUTPUT_NAME },
				XISO_SOURCE,
			),
		).toThrow();
	});

	// `FormatOptions::Zar::output_name` has no `#[serde(default)]` on the
	// Rust side (unlike xiso's, which only becomes required once `split`
	// is on), so an omitted outputName is a hard error, same as ciso/cci.
	it('throws when outputName is omitted', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const readFn = makeReadFn(iso);
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				// @ts-expect-error - outputName deliberately omitted to exercise
				// the serde "missing field" error path.
				{ format: 'zar' },
				XISO_SOURCE,
			),
		).toThrow();
	});

	// `source` is required, not optional, for every target format - an
	// omitted source never falls back to assuming xiso.
	it('throws when source is omitted, instead of silently assuming xiso', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const readFn = makeReadFn(iso);
		expect(() =>
			// @ts-expect-error - source is intentionally omitted to assert
			// the runtime contract for callers not using TS.
			ConversionSession.open(readFn, iso.length, {
				format: 'zar',
				outputName: OUTPUT_NAME,
			}),
		).toThrow(/source format must be resolved/);
	});
});

describe('ConversionSession(zar) with minimal fixture', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);

	it('detectFormat resolves this fixture as xiso (the source shape zar conversion consumes)', () => {
		expect(detectFormat(readFn, iso.length)).toBe('xiso');
	});

	it('opens without throwing', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'zar', outputName: OUTPUT_NAME },
			XISO_SOURCE,
		);
		session.free();
	});

	it('is not done immediately', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'zar', outputName: OUTPUT_NAME },
			XISO_SOURCE,
		);
		expect(session.isDone()).toBe(false);
		session.free();
	});

	it('hashNextPart is a no-op that returns true immediately, same as xiso/extracted', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'zar', outputName: OUTPUT_NAME },
			XISO_SOURCE,
		);
		expect(session.hashNextPart()).toBe(true);
		session.free();
	});

	it("totalUnits equals the fixture's total declared file bytes (default.xbe's DEFAULT_XBE_DECLARED_SIZE), not a sector/part/file count", () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'zar', outputName: OUTPUT_NAME },
			XISO_SOURCE,
		);
		expect(session.totalUnits()).toBe(DEFAULT_XBE_DECLARED_SIZE);
		session.free();
	});

	it('nextChunk returns null once isDone is true', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'zar', outputName: OUTPUT_NAME },
			XISO_SOURCE,
		);
		while (!session.isDone()) {
			session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		}
		expect(session.nextChunk(1)).toBeNull();
		session.free();
	});
});
