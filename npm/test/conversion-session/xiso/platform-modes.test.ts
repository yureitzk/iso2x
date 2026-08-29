import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import { ConversionSession } from '../../../dist/index.js';
import { makeReadFn } from '../../utils/read-fns.js';
import { drain } from '../../utils/session-helpers.js';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

describe('ConversionSession(xiso) zero mode: Xbox vs Xbox 360 platform handling', () => {
	// Zero-padding interior/trailing gaps is expected on original Xbox
	// discs, but should be skipped on Xbox 360 discs. Confirms Zero mode
	// makes that distinction instead of treating every disc as OGX.
	const PAD_SECTORS = 4;
	const PAD_FILL = 0xcd; // distinguishable from both 0x00 and real content

	// Simulates a dump with slack space past the disc's real content.
	function withTrailingPadding(iso: Uint8Array): Uint8Array {
		const out = new Uint8Array(iso.length + PAD_SECTORS * 2048);
		out.set(iso, 0);
		out.fill(PAD_FILL, iso.length);
		return out;
	}

	const ogxIso = withTrailingPadding(
		makeFixture({ titleId: 0x41560001, platform: 'ogx' }),
	);
	const x360Iso = withTrailingPadding(
		makeFixture({ titleId: 0x41560001, platform: 'x360' }),
	);

	it('OGX zero mode zeroes the padding beyond the last used sector', () => {
		const session = ConversionSession.open(
			makeReadFn(ogxIso),
			ogxIso.length,
			{ format: 'xiso', mode: 'zero' },
			XISO_SOURCE,
		);
		const out = drain(session, 64 * 2048);
		const tail = out.slice(out.length - PAD_SECTORS * 2048);
		expect(tail.every((b) => b === 0)).toBe(true);
	});

	it('X360 zero mode leaves that same padding untouched', () => {
		const session = ConversionSession.open(
			makeReadFn(x360Iso),
			x360Iso.length,
			{ format: 'xiso', mode: 'zero' },
			XISO_SOURCE,
		);
		const out = drain(session, 64 * 2048);
		const tail = out.slice(out.length - PAD_SECTORS * 2048);
		expect(tail.every((b) => b === PAD_FILL)).toBe(true);
	});

	it('both platforms still preserve the original file size in zero mode', () => {
		const ogxSession = ConversionSession.open(
			makeReadFn(ogxIso),
			ogxIso.length,
			{
				format: 'xiso',
				mode: 'zero',
			},
			XISO_SOURCE,
		);
		const x360Session = ConversionSession.open(
			makeReadFn(x360Iso),
			x360Iso.length,
			{
				format: 'xiso',
				mode: 'zero',
			},
			XISO_SOURCE,
		);
		expect(ogxSession.totalUnits() * 2048).toBe(ogxIso.length);
		expect(x360Session.totalUnits() * 2048).toBe(x360Iso.length);
		ogxSession.free();
		x360Session.free();
	});
});
