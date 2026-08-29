// TypeScript port of the Xbox 360 profile `Account` file format (XeKeys
// obfuscation + plaintext record layout).
import * as crypto from 'crypto';

// --- XeKeys HMAC/RC4 obfuscation ------------------------

const ACCOUNT_RETAIL_KEY = new Uint8Array([
	0xe1, 0xbc, 0x15, 0x9c, 0x73, 0xb1, 0xea, 0xe9, 0xab, 0x31, 0x70, 0xf3, 0xad,
	0x47, 0xeb, 0xf3,
]);
const ACCOUNT_DEVKIT_KEY = new Uint8Array([
	0xda, 0xb6, 0x9a, 0xd9, 0x8e, 0x28, 0x76, 0x4f, 0x97, 0x7e, 0xe2, 0x48, 0x7e,
	0x4f, 0x3f, 0x68,
]);

function hvpKey(dev: boolean): Uint8Array {
	return dev ? ACCOUNT_DEVKIT_KEY : ACCOUNT_RETAIL_KEY;
}

/** First 16 bytes of HMAC-SHA1(key, data). */
function hmac16(key: Uint8Array, data: Uint8Array): Uint8Array {
	const full = crypto
		.createHmac('sha1', Buffer.from(key))
		.update(Buffer.from(data))
		.digest();
	return new Uint8Array(full.subarray(0, 16));
}

/** Standard RC4 KSA + PRGA, XORed into `data` in place. */
function rc4XorInPlace(data: Uint8Array, key: Uint8Array): void {
	const s = new Uint8Array(256);
	for (let i = 0; i < 256; i++) s[i] = i;
	let j = 0;
	for (let i = 0; i < 256; i++) {
		j = (j + s[i] + key[i % key.length]) & 0xff;
		const tmp = s[i];
		s[i] = s[j];
		s[j] = tmp;
	}
	let i = 0;
	j = 0;
	for (let n = 0; n < data.length; n++) {
		i = (i + 1) & 0xff;
		j = (j + s[i]) & 0xff;
		const tmp = s[i];
		s[i] = s[j];
		s[j] = tmp;
		const k = s[(s[i] + s[j]) & 0xff];
		data[n] ^= k;
	}
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
	if (a.length !== b.length) return false;
	for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
	return true;
}

/**
 * Inverse of `obfuscateAccount`. Returns `null` on HMAC mismatch (wrong
 * key or corrupt data) instead of throwing, so callers can retry with
 * the other key (retail vs devkit).
 */
export function unobfuscateAccount(
	encrypted: Uint8Array,
	dev: boolean,
): Uint8Array | null {
	if (encrypted.length < 0x18) return null;
	const key = hvpKey(dev);
	const baseKey = encrypted.subarray(0, 0x10);
	const body = encrypted.slice(0x10); // mutated in place below

	const rc4Key = hmac16(key, baseKey);
	rc4XorInPlace(body, rc4Key);

	if (!bytesEqual(hmac16(key, body), baseKey)) return null;

	return body.subarray(8);
}

/**
 * Encrypts `plaintext` with the Xbox 360's XeKeys account-file scheme:
 * an 8-byte confounder is prepended, an HMAC-SHA1-derived RC4 key is
 * used to encrypt that combined body, and a header HMAC is stored
 * alongside it for integrity checking on decrypt.
 *
 * `confounder` defaults to 8 random bytes; pass an explicit value for a
 * deterministic fixture.
 */
export function obfuscateAccount(
	plaintext: Uint8Array,
	dev: boolean,
	confounder: Uint8Array = crypto.randomBytes(8),
): Uint8Array {
	if (confounder.length !== 8) {
		throw new Error('obfuscateAccount: confounder must be exactly 8 bytes');
	}
	const key = hvpKey(dev);

	const body = new Uint8Array(8 + plaintext.length);
	body.set(confounder, 0);
	body.set(plaintext, 8);

	const headerKey = hmac16(key, body);
	const rc4Key = hmac16(key, headerKey);
	rc4XorInPlace(body, rc4Key);

	const out = new Uint8Array(0x10 + body.length);
	out.set(headerKey, 0);
	out.set(body, 0x10);
	return out;
}

// --- Plaintext record layout ------------------------------

/** Plaintext record size in bytes. */
export const ACCOUNT_RECORD_LEN = 0x17c;

const offset = {
	liveFlags: 0x00,
	gamertag: 0x08, // 32 bytes, UTF-16BE, 16 chars
	xuidOnline: 0x28,
	cachedUserFlags: 0x30,
	onlineServiceNetworkId: 0x34,
	passcode: 0x38, // 4 bytes
	onlineDomain: 0x3c, // 20 bytes ascii
	onlineKerberosRealm: 0x50, // 24 bytes ascii
	onlineKey: 0x68, // 16 bytes
	userPassportMembername: 0x78, // 114 bytes ascii
	userPassportPassword: 0xea, // 32 bytes ascii
	ownerPassportMembername: 0x10a, // 114 bytes ascii
} as const;

/** Bit flags for the account record's `liveFlags` field. */
export const liveFlag = {
	passwordProtected: 0x1000_0000,
	xboxLiveEnabled: 0x2000_0000,
	recovering: 0x4000_0000,
} as const;

export interface AccountRecord {
	liveFlags?: number;
	gamertag?: string;
	xuidOnline?: bigint;
	cachedUserFlags?: number;
	onlineServiceNetworkId?: number;
	/** 4 bytes. Defaults to all-zero if omitted. */
	passcode?: Uint8Array;
	onlineDomain?: string;
	onlineKerberosRealm?: string;
	/** 16 bytes. Defaults to all-zero if omitted. */
	onlineKey?: Uint8Array;
	userPassportMembername?: string;
	userPassportPassword?: string;
	ownerPassportMembername?: string;
}

/** Parsed record with every field populated - what `parseAccountRecord`
 * returns, regardless of which fields the original `AccountRecord` set
 * explicitly. */
export interface ParsedAccountRecord {
	liveFlags: number;
	gamertag: string;
	xuidOnline: bigint;
	cachedUserFlags: number;
	onlineServiceNetworkId: number;
	passcode: Uint8Array;
	onlineDomain: string;
	onlineKerberosRealm: string;
	onlineKey: Uint8Array;
	userPassportMembername: string;
	userPassportPassword: string;
	ownerPassportMembername: string;
}

/** Big-endian UTF-16, null-padded to `byteLen`. `charCodeAt` only produces
 * correct UTF-16 code units for BMP characters, which is all these
 * fixtures use (ASCII-range gamertags). */
function writeUtf16BeFixed(
	view: DataView,
	base: number,
	byteLen: number,
	s: string,
): void {
	const maxUnits = Math.floor(byteLen / 2);
	const n = Math.min(s.length, maxUnits);
	for (let i = 0; i < n; i++)
		view.setUint16(base + i * 2, s.charCodeAt(i), false);
}

/** Stops at the first null code unit. */
function readUtf16BeFixed(
	view: DataView,
	base: number,
	byteLen: number,
): string {
	let s = '';
	for (let i = 0; i < byteLen; i += 2) {
		const unit = view.getUint16(base + i, false);
		if (unit === 0) break;
		s += String.fromCharCode(unit);
	}
	return s;
}

/** Truncates/null-pads to `len` bytes. */
function writeAsciiFixed(
	buf: Uint8Array,
	base: number,
	len: number,
	s: string,
): void {
	const bytes = new TextEncoder().encode(s);
	const n = Math.min(bytes.length, len);
	buf.set(bytes.subarray(0, n), base);
}

/** Stops at the first null byte. */
function readAsciiFixed(buf: Uint8Array, base: number, len: number): string {
	let end = base + len;
	for (let i = base; i < base + len; i++) {
		if (buf[i] === 0) {
			end = i;
			break;
		}
	}
	return new TextDecoder().decode(buf.subarray(base, end));
}

/** Builds a plaintext `ACCOUNT_RECORD_LEN`-byte record - the input to
 * `obfuscateAccount`. Unset fields are zeroed/empty. */
export function buildAccountRecord(fields: AccountRecord = {}): Uint8Array {
	const buf = new Uint8Array(ACCOUNT_RECORD_LEN);
	const view = new DataView(buf.buffer);

	view.setUint32(offset.liveFlags, fields.liveFlags ?? 0, false);
	writeUtf16BeFixed(view, offset.gamertag, 32, fields.gamertag ?? '');
	view.setBigUint64(offset.xuidOnline, fields.xuidOnline ?? 0n, false);
	view.setUint32(offset.cachedUserFlags, fields.cachedUserFlags ?? 0, false);
	view.setUint32(
		offset.onlineServiceNetworkId,
		fields.onlineServiceNetworkId ?? 0,
		false,
	);
	if (fields.passcode) buf.set(fields.passcode.subarray(0, 4), offset.passcode);
	writeAsciiFixed(buf, offset.onlineDomain, 20, fields.onlineDomain ?? '');
	writeAsciiFixed(
		buf,
		offset.onlineKerberosRealm,
		24,
		fields.onlineKerberosRealm ?? '',
	);
	if (fields.onlineKey)
		buf.set(fields.onlineKey.subarray(0, 16), offset.onlineKey);
	writeAsciiFixed(
		buf,
		offset.userPassportMembername,
		114,
		fields.userPassportMembername ?? '',
	);
	writeAsciiFixed(
		buf,
		offset.userPassportPassword,
		32,
		fields.userPassportPassword ?? '',
	);
	writeAsciiFixed(
		buf,
		offset.ownerPassportMembername,
		114,
		fields.ownerPassportMembername ?? '',
	);

	return buf;
}

/** Inverse of `buildAccountRecord` - parses a plaintext
 * `ACCOUNT_RECORD_LEN`-byte record (typically the output of
 * `unobfuscateAccount`). */
export function parseAccountRecord(buf: Uint8Array): ParsedAccountRecord {
	if (buf.length !== ACCOUNT_RECORD_LEN) {
		throw new Error(
			`parseAccountRecord: expected ${ACCOUNT_RECORD_LEN} bytes, got ${buf.length}`,
		);
	}
	const view = new DataView(buf.buffer, buf.byteOffset, buf.length);

	return {
		liveFlags: view.getUint32(offset.liveFlags, false),
		gamertag: readUtf16BeFixed(view, offset.gamertag, 32),
		xuidOnline: view.getBigUint64(offset.xuidOnline, false),
		cachedUserFlags: view.getUint32(offset.cachedUserFlags, false),
		onlineServiceNetworkId: view.getUint32(offset.onlineServiceNetworkId, false),
		passcode: buf.slice(offset.passcode, offset.passcode + 4),
		onlineDomain: readAsciiFixed(buf, offset.onlineDomain, 20),
		onlineKerberosRealm: readAsciiFixed(buf, offset.onlineKerberosRealm, 24),
		onlineKey: buf.slice(offset.onlineKey, offset.onlineKey + 16),
		userPassportMembername: readAsciiFixed(
			buf,
			offset.userPassportMembername,
			114,
		),
		userPassportPassword: readAsciiFixed(buf, offset.userPassportPassword, 32),
		ownerPassportMembername: readAsciiFixed(
			buf,
			offset.ownerPassportMembername,
			114,
		),
	};
}

/** `buildAccountRecord` + `obfuscateAccount` in one call - the bytes to
 * hand to `makeStfsFixture({ fileName: 'Account', fileBytes: ... })` for
 * an STFS profile-package fixture. */
export function makeAccountFileBytes(
	fields: AccountRecord = {},
	dev: boolean = false,
): Uint8Array {
	return obfuscateAccount(buildAccountRecord(fields), dev);
}

/** 8-byte big-endian XUID, the shape `newXuid`/`profileId` overrides and
 * `xuid_online` all share. */
export function xuidBytes(xuid: bigint): Uint8Array {
	const out = new Uint8Array(8);
	new DataView(out.buffer).setBigUint64(0, xuid, false);
	return out;
}
