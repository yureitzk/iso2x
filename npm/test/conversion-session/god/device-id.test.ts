import { describe, it, expect, beforeAll } from 'vitest';
import * as crypto from 'crypto';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import {
	makeSyntheticKeyvault,
	bnQwSwap,
} from '../../utils/fixtures/keyvault.js';
import { ConversionSession } from '../../../dist/index.js';
import { makeReadFn } from '../../utils/read-fns.js';
import { driveToGodHeader as driveToHeader } from '../../utils/session-helpers.js';
import {
	STFS_WRITE_DEVICE_ID_OFFSET as DEVICE_ID_OFFSET,
	STFS_WRITE_DEVICE_ID_LEN as DEVICE_ID_LEN,
} from '../../utils/fixtures/stfs.js';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

// Sequential, not random - enough to prove the exact bytes round-trip,
// and keeps a failing assertion's diff readable.
function makeDeviceId(): Uint8Array {
	return Uint8Array.from({ length: DEVICE_ID_LEN }, (_, i) => i + 1);
}

function deviceIdField(header: Uint8Array): Uint8Array {
	return header.slice(DEVICE_ID_OFFSET, DEVICE_ID_OFFSET + DEVICE_ID_LEN);
}

describe('ConversionSession(god) rejects an invalid deviceId', () => {
	const iso = makeFixture({ titleId: 0x44450001 });
	const readFn = makeReadFn(iso);

	it('throws for a deviceId shorter than 20 bytes', () => {
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'god', deviceId: new Uint8Array(19) },
				XISO_SOURCE,
			),
		).toThrow(/20 bytes/);
	});

	it('throws for a deviceId longer than 20 bytes', () => {
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'god', deviceId: new Uint8Array(21) },
				XISO_SOURCE,
			),
		).toThrow(/20 bytes/);
	});

	it('throws for an empty (but present) deviceId', () => {
		// Deliberately distinct from omitting the field entirely - an
		// empty Uint8Array still deserializes to `Some(vec![])`, not
		// `None`, so this must fail the length check rather than silently
		// behaving like the field was never passed.
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'god', deviceId: new Uint8Array(0) },
				XISO_SOURCE,
			),
		).toThrow(/20 bytes/);
	});
});

describe('ConversionSession(god) deviceId', () => {
	// Default platform is 'ogx' (see xsf.ts) - deliberately not x360/XEX
	// here, to keep this block's coverage independent of signingKey's
	// GamesOnDemand-only restriction. The combined-with-signingKey cases
	// below use their own x360 fixture instead.
	const iso = makeFixture({ titleId: 0x44450002 });
	const readFn = makeReadFn(iso);

	it('omitting deviceId leaves the header field zeroed', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		const field = deviceIdField(header);
		expect(Array.from(field).every((b) => b === 0)).toBe(true);
	});

	it('passing deviceId writes it verbatim at offset 0x3fd', () => {
		const deviceId = makeDeviceId();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', deviceId },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(Array.from(deviceIdField(header))).toEqual(Array.from(deviceId));
	});

	it('is accepted for an OGX (XboxOriginal) source, unlike signingKey', () => {
		// Contrast with signing.test.ts's "console-signing is rejected for
		// Original Xbox sources" block - deviceId has no such restriction.
		const deviceId = makeDeviceId();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', deviceId },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getUint32(0x344, false)).toBe(0x5000); // XboxOriginal
		expect(Array.from(deviceIdField(header))).toEqual(Array.from(deviceId));
	});

	it('changes only the Device ID field and the header digest at 0x32c - every other byte matches the deviceId-less conversion', () => {
		// The header SHA1 digest at 0x32C covers everything from 0x344
		// onward (see ConHeaderBuilder::write_digest), which includes the
		// Device ID field itself - so the digest is expected to differ
		// too, not just the 20 bytes at 0x3fd.
		const withoutSession = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		const withoutHeader = driveToHeader(withoutSession);
		withoutSession.free();

		const deviceId = makeDeviceId();
		const withSession = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', deviceId },
			XISO_SOURCE,
		);
		const withHeader = driveToHeader(withSession);
		withSession.free();

		expect(withHeader.length).toBe(withoutHeader.length);
		const untouched = (i: number) =>
			(i >= DEVICE_ID_OFFSET && i < DEVICE_ID_OFFSET + DEVICE_ID_LEN) ||
			(i >= 0x32c && i < 0x32c + 0x14);
		for (let i = 0; i < withHeader.length; i++) {
			if (untouched(i)) continue;
			expect(withHeader[i]).toBe(withoutHeader[i]);
		}
		expect(Array.from(withHeader.slice(0x32c, 0x32c + 0x14))).not.toEqual(
			Array.from(withoutHeader.slice(0x32c, 0x32c + 0x14)),
		);
	});

	it('two different deviceId values produce two different header digests', () => {
		const first = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', deviceId: makeDeviceId() },
			XISO_SOURCE,
		);
		const firstHeader = driveToHeader(first);
		first.free();

		const otherDeviceId = Uint8Array.from(makeDeviceId(), (b) => b ^ 0xff);
		const second = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', deviceId: otherDeviceId },
			XISO_SOURCE,
		);
		const secondHeader = driveToHeader(second);
		second.free();

		expect(Array.from(firstHeader.slice(0x32c, 0x32c + 0x14))).not.toEqual(
			Array.from(secondHeader.slice(0x32c, 0x32c + 0x14)),
		);
	});
});

describe('ConversionSession(god) deviceId combined with signingKey', () => {
	// x360 (XEX) source - the only platform signingKey is accepted for.
	const iso = makeFixture({ titleId: 0x44450003, platform: 'x360' });
	const readFn = makeReadFn(iso);

	it('applies with no signingKey, on an unsigned ("LIVE") package', () => {
		const deviceId = makeDeviceId();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', deviceId },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(Array.from(header.slice(0, 4))).toEqual([
			0x4c,
			0x49,
			0x56,
			0x45, // 'LIVE'
		]);
		expect(Array.from(deviceIdField(header))).toEqual(Array.from(deviceId));
	});

	it('also applies on a console-signed ("CON ") package', () => {
		const { kv } = makeSyntheticKeyvault();
		const deviceId = makeDeviceId();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', signingKey: kv, deviceId },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(Array.from(header.slice(0, 4))).toEqual([
			0x43,
			0x4f,
			0x4e,
			0x20, // 'CON '
		]);
		expect(Array.from(deviceIdField(header))).toEqual(Array.from(deviceId));
	});

	it('the embedded signature still verifies with deviceId set - Device ID falls inside the signed digest, not outside it', () => {
		const { kv, publicKey } = makeSyntheticKeyvault();
		const deviceId = makeDeviceId();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', signingKey: kv, deviceId },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		// Same signed-region/wire-format unswap as signing.test.ts's
		// "embedded signature verifies" case.
		const signedRegion = header.slice(0x22c, 0x344);
		const wireSignature = header.slice(0x1ac, 0x1ac + 0x80);
		const signature = wireSignature.slice();
		bnQwSwap(signature, true);
		const ok = crypto.verify(
			'sha1',
			Buffer.from(signedRegion),
			publicKey,
			Buffer.from(signature),
		);
		expect(ok).toBe(true);
	});

	it('a signature stays valid but the header still differs between two different deviceId values', () => {
		// Guards against a signature that verifies only because the
		// signer forgot to fold deviceId into what it covers - if that
		// were true, two different deviceId values would still produce
		// the exact same (still "valid") header.
		const { kv } = makeSyntheticKeyvault();
		const a = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', signingKey: kv, deviceId: makeDeviceId() },
			XISO_SOURCE,
		);
		const headerA = driveToHeader(a);
		a.free();

		const otherDeviceId = Uint8Array.from(makeDeviceId(), (b) => b ^ 0xff);
		const b = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', signingKey: kv, deviceId: otherDeviceId },
			XISO_SOURCE,
		);
		const headerB = driveToHeader(b);
		b.free();

		expect(Array.from(headerA)).not.toEqual(Array.from(headerB));
	});
});
