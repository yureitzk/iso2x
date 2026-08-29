import * as crypto from 'crypto';

/**
 * Offsets into a raw Xbox 360 console keyvault dump. Ported by hand from
 * the Rust signing implementation and kept in sync manually, since it
 * isn't exposed over the wasm boundary.
 *
 * Certificate structure at CERTIFICATE_OFFSET ("Console Security
 * Certificate"): https://free60.org/System-Software/Formats/STFS/#signatures
 */
const EXPONENT_OFFSET = 0x29c;
const MODULUS_OFFSET = 0x2a8;
const CERTIFICATE_OFFSET = 0x9c8;
const CERTIFICATE_LEN = 0x1a8;
const MODULUS_LEN = 0x80; // 1024-bit console signing key
const PRIME_LEN = 0x40;
const PRIVATE_KEY_BLOCK_LEN = MODULUS_LEN + PRIME_LEN * 5; // n, p, q, dp, dq, qinv

/**
 * The base64url big-integer fields read off an RSA private key's JWK
 * export. Declared locally rather than using `crypto.JsonWebKey` since
 * its shape has varied across `@types/node` versions.
 */
interface RsaPrivateJwk {
	n?: string;
	p?: string;
	q?: string;
}

/**
 * Xbox 360 bignums are stored as a reversed sequence of 8-byte qwords
 * relative to plain big-endian - swaps qword `i` with qword `N-1-i` in
 * place. `reverseBytes` additionally flips the byte order within each
 * qword; used both to encode plain big-endian P/Q/modulus into wire
 * format here, and (with `reverseBytes: true`) to decode a signature
 * back into one a standard RSA verifier understands.
 */
export function bnQwSwap(data: Uint8Array, reverseBytes: boolean): void {
	if (data.length % 8 !== 0) {
		throw new Error('bnQwSwap: length must be a multiple of 8');
	}
	const len = data.length;
	for (let i = 0; i < Math.floor(len / 2); i += 8) {
		for (let k = 0; k < 8; k++) {
			const a = i + k;
			const b = len - i - 8 + k;
			const tmp = data[a];
			data[a] = data[b];
			data[b] = tmp;
		}
	}
	if (reverseBytes) {
		for (let i = 0; i < len; i += 8) {
			for (let k = 0; k < 4; k++) {
				const a = i + k;
				const b = i + 7 - k;
				const tmp = data[a];
				data[a] = data[b];
				data[b] = tmp;
			}
		}
	}
}

/** Left-pads `bytes` with zeros to exactly `len` bytes, for a JWK export
 * that comes back a byte short because the leading byte was zero. */
function padLeft(bytes: Uint8Array, len: number): Uint8Array {
	if (bytes.length === len) return bytes;
	if (bytes.length > len) {
		throw new Error(`padLeft: ${bytes.length} bytes doesn't fit in ${len}`);
	}
	const out = new Uint8Array(len);
	out.set(bytes, len - bytes.length);
	return out;
}

export interface SyntheticKeyvault {
	/** The full raw keyvault buffer. */
	kv: Uint8Array;
	/** The real RSA public key backing it, for verifying signatures
	 * produced with the corresponding `SigningKey` against a standard RSA
	 * verifier. */
	publicKey: crypto.KeyObject;
	/** The 0x1A8-byte certificate blob embedded at `CERTIFICATE_OFFSET`,
	 * for asserting it round-trips byte-for-byte into a signed header. */
	certificate: Uint8Array;
}

/**
 * Builds a synthetic keyvault buffer around a real 1024-bit RSA key,
 * encoded the way the Xbox 360 console signing key format expects
 * (P/Q components byte-reversed via `bnQwSwap`).
 *
 * dp/dq/qinv are left zeroed: they're re-derivable from P/Q/exponent via
 * standard RSA-CRT math, so nothing in this codebase needs to read them
 * from the keyvault directly.
 */
export function makeSyntheticKeyvault(): SyntheticKeyvault {
	const { publicKey, privateKey } = crypto.generateKeyPairSync('rsa', {
		modulusLength: 1024,
		publicExponent: 0x10001,
	});
	const jwk = privateKey.export({ format: 'jwk' }) as RsaPrivateJwk;
	const toBytes = (b64url: string) =>
		new Uint8Array(Buffer.from(b64url, 'base64url'));
	const n = padLeft(toBytes(jwk.n!), MODULUS_LEN);
	const p = padLeft(toBytes(jwk.p!), PRIME_LEN);
	const q = padLeft(toBytes(jwk.q!), PRIME_LEN);

	const kv = new Uint8Array(CERTIFICATE_OFFSET + CERTIFICATE_LEN);
	const view = new DataView(kv.buffer);
	view.setUint32(EXPONENT_OFFSET, 0x10001, false); // big-endian u32

	const block = new Uint8Array(PRIVATE_KEY_BLOCK_LEN);
	block.set(n, 0);
	block.set(p, MODULUS_LEN);
	block.set(q, MODULUS_LEN + PRIME_LEN);
	bnQwSwap(block.subarray(0, MODULUS_LEN), false);
	bnQwSwap(block.subarray(MODULUS_LEN, MODULUS_LEN + PRIME_LEN), false);
	bnQwSwap(
		block.subarray(MODULUS_LEN + PRIME_LEN, MODULUS_LEN + 2 * PRIME_LEN),
		false,
	);
	kv.set(block, MODULUS_OFFSET);

	// Filler, not real X.509 bytes - parse() carries the certificate
	// through verbatim, never parses it.
	const certificate = new Uint8Array(CERTIFICATE_LEN);
	for (let i = 0; i < CERTIFICATE_LEN; i++) certificate[i] = i & 0xff;
	kv.set(certificate, CERTIFICATE_OFFSET);

	return { kv, publicKey, certificate };
}
