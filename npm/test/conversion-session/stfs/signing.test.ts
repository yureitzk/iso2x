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
import {
	driveToStfsHeader as driveToHeader,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

describe('ConversionSession(stfs) rejects an invalid signingKey', () => {
	const iso = makeFixture({ titleId: 0x53190001 });
	const readFn = makeReadFn(iso);

	it('throws for a buffer too short to contain a certificate', () => {
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'stfs', signingKey: new Uint8Array(10) },
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('throws for a buffer long enough for a certificate but not a private key', () => {
		// Long enough to pass the CERTIFICATE bounds check, short of
		// MODULUS + PRIVATE_KEY_BLOCK_LEN (0x2a8 + 0x2a0 = 0x548).
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'stfs', signingKey: new Uint8Array(0x500) },
				XISO_SOURCE,
			),
		).toThrow();
	});
});

describe('ConversionSession(stfs) console-signing', () => {
	// Unlike `god`, `StfsWriteSession::open_inner` never rejects a
	// signingKey based on source platform - content-type resolution is
	// unaffected by signing_key. An OGX source is used here specifically
	// to confirm that.
	const iso = makeFixture({ titleId: 0x53190001 });
	const readFn = makeReadFn(iso);

	it('signed and unsigned sessions both open and drain without throwing', () => {
		const { kv } = makeSyntheticKeyvault();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs', signingKey: kv },
			XISO_SOURCE,
		);
		driveToHeader(session);
		while (!session.isDone()) session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		session.free();
	});

	it('omitting signingKey still produces the "CON " magic - unlike god, build_header writes MAGIC_CON unconditionally, signed or not', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs' },
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
	});

	it('omitting signingKey leaves the certificate region at offset 0x4 zeroed', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs' },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		const certificateRegion = header.slice(0x4, 0x4 + 0x1a8);
		expect(certificateRegion.every((b) => b === 0)).toBe(true);
	});

	it('embeds the keyvault certificate verbatim at offset 0x4', () => {
		const { kv, certificate } = makeSyntheticKeyvault();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs', signingKey: kv },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(header.slice(0x4, 0x4 + certificate.length)).toEqual(certificate);
	});

	it('writes a single full-license (0xF0) entry naming the console at 0x22C', () => {
		const { kv, certificate } = makeSyntheticKeyvault();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs', signingKey: kv },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(header[0x22c]).toBe(0xf0);
		expect(header.slice(0x22c + 3, 0x22c + 8)).toEqual(certificate.slice(2, 7));
	});

	it('the embedded signature verifies against the real RSA public key', () => {
		const { kv, publicKey } = makeSyntheticKeyvault();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs', signingKey: kv },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		// Signed region is [0x22C, 0x344) - license table + digest, shared
		// unchanged between the `god` and `stfs` targets.
		const signedRegion = header.slice(0x22c, 0x344);
		// Signature lives at [0x1AC, 0x1AC + 0x80) - un-swap it out of
		// Xbox 360 wire format back into a plain big-endian PKCS1v1.5
		// signature (bnQwSwap with reverseBytes: true is its own inverse).
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

	it('a signature produced with one key does not verify against a different key', () => {
		const { kv } = makeSyntheticKeyvault();
		const { publicKey: unrelatedPublicKey } = makeSyntheticKeyvault();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs', signingKey: kv },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		const signedRegion = header.slice(0x22c, 0x344);
		const signature = header.slice(0x1ac, 0x1ac + 0x80).slice();
		bnQwSwap(signature, true);
		const ok = crypto.verify(
			'sha1',
			Buffer.from(signedRegion),
			unrelatedPublicKey,
			Buffer.from(signature),
		);
		expect(ok).toBe(false);
	});

	it('the signature covers the real topHashTableHash, not a zeroed one - a signed and unsigned header of the same content differ under [0x22C, 0x344)', () => {
		const { kv } = makeSyntheticKeyvault();
		const unsignedSession = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs' },
			XISO_SOURCE,
		);
		const unsignedHeader = driveToHeader(unsignedSession);
		unsignedSession.free();

		const signedSession = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs', signingKey: kv },
			XISO_SOURCE,
		);
		const signedHeader = driveToHeader(signedSession);
		signedSession.free();

		expect(signedHeader.slice(0x22c, 0x344)).not.toEqual(
			unsignedHeader.slice(0x22c, 0x344),
		);
	});
});
