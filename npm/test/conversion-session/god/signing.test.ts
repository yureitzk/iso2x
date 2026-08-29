import { describe, it, expect, beforeAll } from 'vitest';
import * as crypto from 'crypto';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import {
	makeSyntheticKeyvault,
	bnQwSwap,
} from '../../utils/fixtures/keyvault.js';
import { ConversionSession, inspectSource } from '../../../dist/index.js';
import { makeReadFn, nullReadFn } from '../../utils/read-fns.js';
import {
	convertXisoFixtureToGodParts,
	driveAllChunks,
	driveHashing,
	driveToGodHeader as driveToHeader,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import { GOD_SOURCE, XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

describe('ConversionSession(god) rejects an invalid signingKey', () => {
	const iso = makeFixture({ titleId: 0x53190001, platform: 'x360' });
	const readFn = makeReadFn(iso);

	it('throws for a buffer too short to contain a certificate', () => {
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'god', signingKey: new Uint8Array(10) },
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
				{ format: 'god', signingKey: new Uint8Array(0x500) },
				XISO_SOURCE,
			),
		).toThrow();
	});
});

describe('ConversionSession(god) console-signing', () => {
	// x360 (XEX) source - the only platform signing is accepted for. See
	// the OGX-rejection block further down for the other half of this.
	const iso = makeFixture({ titleId: 0x53190001, platform: 'x360' });
	const readFn = makeReadFn(iso);

	it('signed and unsigned sessions both open and drain without throwing', () => {
		const { kv } = makeSyntheticKeyvault();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', signingKey: kv },
			XISO_SOURCE,
		);
		driveToHeader(session);
		session.free();
	});

	it('omitting signingKey produces the default unsigned ("LIVE") header', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
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
	});

	it('passing signingKey produces a "CON " header instead of "LIVE"', () => {
		const { kv } = makeSyntheticKeyvault();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', signingKey: kv },
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

	it('embeds the keyvault certificate verbatim at offset 0x4', () => {
		const { kv, certificate } = makeSyntheticKeyvault();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', signingKey: kv },
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
			{ format: 'god', signingKey: kv },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(header[0x22c]).toBe(0xf0);
		expect(header.slice(0x22c + 3, 0x22c + 8)).toEqual(certificate.slice(2, 7));
	});

	it('content type at 0x344 is InstalledGame (0x4000), not GamesOnDemand', () => {
		const { kv } = makeSyntheticKeyvault();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', signingKey: kv },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getUint32(0x344, false)).toBe(0x4000);
	});

	it("outputManifest's header entry folder name reflects the InstalledGame content type, not GamesOnDemand", () => {
		const { kv } = makeSyntheticKeyvault();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', signingKey: kv },
			XISO_SOURCE,
		);
		driveHashing(session);
		const manifest = session.outputManifest();
		while (!session.isDone()) session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		session.free();
		const header = manifest[manifest.length - 1];
		// <titleId>/<contentType>/<mediaId> - middle segment is content type.
		expect(header.name.split('/')[1]).toBe('00004000');
	});

	it('the embedded signature verifies against the real RSA public key', () => {
		const { kv, publicKey } = makeSyntheticKeyvault();
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', signingKey: kv },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		// Signed region is [0x22C, 0x344) - license table + digest, per
		// ConHeaderBuilder::finalize_signed / SIGNED_REGION.
		const signedRegion = header.slice(0x22c, 0x344);
		// Signature lives at [0x1AC, 0x1AC + 0x80) - un-swap it out of
		// Xbox 360 wire format back into a plain big-endian PKCS1v1.5
		// signature a standard verifier understands (bn_qw_swap with
		// reverseBytes: true is its own inverse - see
		// utils/fixtures/keyvault.ts's `bnQwSwap`).
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
			{ format: 'god', signingKey: kv },
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

	it('console-signing only changes the header - every Data%04d/MHT chunk is byte-identical to the unsigned conversion', () => {
		const { kv } = makeSyntheticKeyvault();
		const unsignedSession = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		const unsignedChunks = driveAllChunks(unsignedSession);
		unsignedSession.free();
		const signedSession = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god', signingKey: kv },
			XISO_SOURCE,
		);
		const signedChunks = driveAllChunks(signedSession);
		signedSession.free();

		// Signing never changes part_count/block_count/data_size, so both
		// sessions must emit the same number of chunks (N part-payload
		// chunks + 1 trailing header).
		expect(signedChunks.length).toBe(unsignedChunks.length);
		expect(signedChunks.length).toBeGreaterThan(1);
		// Every chunk except the last (MHTs + subpart data) comes purely from
		// `backend`/hashing, which `signing_key` never touches - only the
		// final chunk (the CON/LIVE header) is allowed to differ.
		for (let i = 0; i < unsignedChunks.length - 1; i++) {
			expect(Array.from(signedChunks[i])).toEqual(Array.from(unsignedChunks[i]));
		}
		const unsignedHeader = unsignedChunks[unsignedChunks.length - 1];
		const signedHeader = signedChunks[signedChunks.length - 1];
		expect(Array.from(signedHeader)).not.toEqual(Array.from(unsignedHeader));
		expect(Array.from(unsignedHeader.slice(0, 4))).toEqual([
			0x4c,
			0x49,
			0x56,
			0x45, // 'LIVE'
		]);
		expect(Array.from(signedHeader.slice(0, 4))).toEqual([
			0x43,
			0x4f,
			0x4e,
			0x20, // 'CON '
		]);
	});
});

describe('ConversionSession(god) console-signing is rejected for Original Xbox sources', () => {
	// Default platform is 'ogx' - see xsf.ts.
	const iso = makeFixture({ titleId: 0x53190002 });
	const readFn = makeReadFn(iso);

	it('throws mentioning GamesOnDemand when signingKey is passed for an OGX source', () => {
		const { kv } = makeSyntheticKeyvault();
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'god', signingKey: kv },
				XISO_SOURCE,
			),
		).toThrow(/GamesOnDemand/);
	});

	it('still opens and produces XboxOriginal-shaped output when signingKey is omitted', () => {
		// The OGX-rejection above must only fire when a key is actually
		// supplied - plain unsigned OGX->god must be completely unaffected
		// by any of this.
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'god' },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		const view = new DataView(header.buffer, header.byteOffset, header.length);
		expect(view.getUint32(0x344, false)).toBe(0x5000); // XboxOriginal
	});
});

describe('ConversionSession(god source) a signed (NXE) god converts to a regular god', () => {
	const iso = makeFixture({ titleId: 0x6a6a0024, platform: 'x360' });

	it('feeding a signed god in as the source and converting to god (no signingKey) produces a regular "LIVE" god', () => {
		const { kv } = makeSyntheticKeyvault();
		const { dataParts: signedGodParts, headerPart } =
			convertXisoFixtureToGodParts(iso, {
				format: 'god',
				signingKey: kv,
			});

		// Confirms this really is a signed InstalledGame source (not just a
		// same-shaped LIVE package) before using it below - exercises the
		// read-side content_type_override, cross-checked against the
		// write-side 0x344/manifest-name assertions earlier in this file.
		const sourceInfo = inspectSource(nullReadFn, iso.length, {
			source: GOD_SOURCE.source,
			parts: [...signedGodParts, headerPart],
		});
		expect(sourceInfo.contentType).toBe('installedGame');

		const session = ConversionSession.open(
			nullReadFn,
			iso.length,
			{ format: 'god' },
			{
				source: GOD_SOURCE.source,
				parts: signedGodParts, // header intentionally omitted here - GodSource only ever needs the Data parts to convert
			},
		);
		const header = driveToHeader(session);
		session.free();
		expect(Array.from(header.slice(0, 4))).toEqual([
			0x4c,
			0x49,
			0x56,
			0x45, // 'LIVE'
		]);
	});
});
