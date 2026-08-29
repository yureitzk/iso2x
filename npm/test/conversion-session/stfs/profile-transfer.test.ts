import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import {
	makeStfsFixture,
	STFS_WRITE_DEFAULT_HEADER_SIZE as DEFAULT_HEADER_SIZE,
	STFS_WRITE_PROFILE_ID_OFFSET as PROFILE_ID_OFFSET,
	STFS_WRITE_PROFILE_ID_LEN as PROFILE_ID_LEN,
	STFS_WRITE_ONLINE_CREATOR_OFFSET as ONLINE_CREATOR_OFFSET,
	STFS_WRITE_ONLINE_CREATOR_LEN as ONLINE_CREATOR_LEN,
} from '../../utils/fixtures/stfs.js';
import {
	obfuscateAccount,
	unobfuscateAccount,
	buildAccountRecord,
	parseAccountRecord,
	makeAccountFileBytes,
	xuidBytes,
	ACCOUNT_RECORD_LEN,
	liveFlag,
} from '../../utils/fixtures/account.js';
import type { AccountRecord } from '../../utils/fixtures/account.js';
import { ConversionSession } from '../../../dist/index.js';
import { makeReadFn } from '../../utils/read-fns.js';
import {
	driveAndDrain,
	UNBOUNDED_CHUNK_SIZE,
} from '../../utils/session-helpers.js';
import { STFS_SOURCE, XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

// Sanity: confirms the TS port of the obfuscate/account record format
// (fixtures/account.ts) round-trips on its own, independent of anything
// the wasm side does. Every test below that trusts
// unobfuscateAccount/parseAccountRecord to read back a wasm-produced
// Account file relies on this being correct - if this block ever fails,
// suspect the fixture port before suspecting profileTransfer itself.
describe('Account fixture port sanity', () => {
	it('obfuscateAccount/unobfuscateAccount round-trip an arbitrary plaintext buffer', () => {
		const plaintext = new Uint8Array(ACCOUNT_RECORD_LEN).fill(0x42);
		const wire = obfuscateAccount(plaintext, false);
		expect(unobfuscateAccount(wire, false)).toEqual(plaintext);
	});

	it('unobfuscateAccount returns null for the wrong key (retail vs devkit)', () => {
		const plaintext = new Uint8Array(ACCOUNT_RECORD_LEN).fill(0x11);
		const wire = obfuscateAccount(plaintext, false);
		expect(unobfuscateAccount(wire, true)).toBeNull();
	});

	it('unobfuscateAccount returns null for corrupted (HMAC-mismatching) bytes', () => {
		const plaintext = new Uint8Array(ACCOUNT_RECORD_LEN).fill(0x22);
		const wire = obfuscateAccount(plaintext, false);
		wire[wire.length - 1] ^= 0xff;
		expect(unobfuscateAccount(wire, false)).toBeNull();
	});

	it('buildAccountRecord/parseAccountRecord round-trip gamertag and xuidOnline', () => {
		const record = buildAccountRecord({
			gamertag: 'Test Gamer',
			xuidOnline: 0xe000012345678901n,
		});
		expect(record.length).toBe(ACCOUNT_RECORD_LEN);
		const parsed = parseAccountRecord(record);
		expect(parsed.gamertag).toBe('Test Gamer');
		expect(parsed.xuidOnline).toBe(0xe000012345678901n);
	});
});

/**
 * Builds a single-file STFS package whose one file is named "Account"
 * and holds a real obfuscated Account record - the shape `profileTransfer`
 * requires: an already-extracted STFS *profile* package with a
 * root-level Account file, matched case-insensitively.
 */
function makeProfilePackage(record: AccountRecord = {}): Uint8Array {
	// `titleId`/`version` are irrelevant here - they only affect the
	// default XEX2 stub, which `fileBytes` replaces entirely.
	const fileBytes = makeAccountFileBytes(record, false);
	return makeStfsFixture({ fileName: 'Account', fileBytes }).bytes;
}

/**
 * Runs a `profileTransfer` session to completion, then re-opens the
 * result as an `extracted` source to recover the mutated `Account`
 * file's bytes. This sidesteps re-deriving the writer's own physical-
 * block placement math by reusing the reader/extraction path instead -
 * the same "round-trip through extracted" technique source.test.ts's
 * own checks use for its default.xex fixture file.
 */
function driveProfileTransferAndExtractAccount(
	source: Uint8Array,
	profileTransfer: Record<string, unknown>,
	extraFormatOptions: Record<string, unknown> = {},
): {
	headerProfileId: Uint8Array;
	headerOnlineCreator: Uint8Array;
	account: ReturnType<typeof parseAccountRecord>;
} {
	const session = ConversionSession.open(
		makeReadFn(source),
		source.length,
		{ format: 'stfs', profileTransfer, ...extraFormatOptions },
		STFS_SOURCE,
	);
	const outBytes = driveAndDrain(session, UNBOUNDED_CHUNK_SIZE);
	const header = outBytes.slice(0, DEFAULT_HEADER_SIZE);
	const headerProfileId = header.slice(
		PROFILE_ID_OFFSET,
		PROFILE_ID_OFFSET + PROFILE_ID_LEN,
	);
	const headerOnlineCreator = header.slice(
		ONLINE_CREATOR_OFFSET,
		ONLINE_CREATOR_OFFSET + ONLINE_CREATOR_LEN,
	);
	const extractedSession = ConversionSession.open(
		makeReadFn(outBytes),
		outBytes.length,
		{ format: 'extracted' },
		STFS_SOURCE,
	);
	const manifest = extractedSession.outputManifest();
	const entry = manifest.find((e) => e.name.toLowerCase() === 'account');
	if (!entry) {
		extractedSession.free();
		throw new Error('extracted output has no Account entry');
	}
	const all = driveAndDrain(extractedSession, 1024 * 1024);
	// Only one file in this fixture, so the whole drained stream is it.
	const obfuscated = all.slice(0, entry.size);
	const plaintext = unobfuscateAccount(obfuscated, false);
	expect(plaintext).not.toBeNull();
	const account = parseAccountRecord(plaintext!);
	return { headerProfileId, headerOnlineCreator, account };
}

describe('ConversionSession(stfs) profileTransfer', () => {
	it('rewrites the gamertag when only newGamertag is given, leaves xuidOnline untouched, and stamps profileId from the account\u2019s (unchanged) XUID', () => {
		const originalXuid = 0x1122334455667788n;
		const source = makeProfilePackage({
			gamertag: 'Old Name',
			xuidOnline: originalXuid,
		});
		const { headerProfileId, account } = driveProfileTransferAndExtractAccount(
			source,
			{ newGamertag: 'New Name' },
		);
		expect(account.gamertag).toBe('New Name');
		expect(account.xuidOnline).toBe(originalXuid);
		expect(headerProfileId).toEqual(xuidBytes(originalXuid));
	});

	it('rewrites xuidOnline when only newXuid is given, leaves gamertag untouched, and profileId reflects the new XUID', () => {
		const newXuid = 0xaabbccddeeff0011n;
		const source = makeProfilePackage({ gamertag: 'Keep Me', xuidOnline: 1n });
		const { headerProfileId, account } = driveProfileTransferAndExtractAccount(
			source,
			{ newXuid: xuidBytes(newXuid) },
		);
		expect(account.gamertag).toBe('Keep Me');
		expect(account.xuidOnline).toBe(newXuid);
		expect(headerProfileId).toEqual(xuidBytes(newXuid));
	});

	it('rewrites both gamertag and xuidOnline when both are given', () => {
		const newXuid = 0x0102030405060708n;
		const source = makeProfilePackage({ gamertag: 'Before', xuidOnline: 1n });
		const { account } = driveProfileTransferAndExtractAccount(source, {
			newGamertag: 'After',
			newXuid: xuidBytes(newXuid),
		});
		expect(account.gamertag).toBe('After');
		expect(account.xuidOnline).toBe(newXuid);
	});

	it('never derives onlineCreator from the mutated account - it stays zero even for an Xbox-Live-enabled account, unless explicitly overridden', () => {
		// An earlier version inferred onlineCreator from
		// xbox_live_enabled(); that hypothesis didn't hold up, so
		// profileTransfer must never touch it on its own.
		const source = makeProfilePackage({
			gamertag: 'Live User',
			xuidOnline: 1n,
			liveFlags: liveFlag.xboxLiveEnabled,
		});
		const { headerOnlineCreator } = driveProfileTransferAndExtractAccount(
			source,
			{
				newGamertag: 'Live User Renamed',
			},
		);
		expect(headerOnlineCreator.every((b) => b === 0)).toBe(true);
	});

	it('an explicit onlineCreator override still applies normally alongside profileTransfer', () => {
		const onlineCreator = new Uint8Array(8).fill(0x77);
		const source = makeProfilePackage({ gamertag: 'X', xuidOnline: 1n });
		const { headerOnlineCreator } = driveProfileTransferAndExtractAccount(
			source,
			{ newGamertag: 'Y' },
			{ onlineCreator },
		);
		expect(headerOnlineCreator).toEqual(onlineCreator);
	});
});

describe('ConversionSession(stfs) profileTransfer error paths', () => {
	it('throws when neither newGamertag nor newXuid is given', () => {
		const source = makeProfilePackage({ gamertag: 'X', xuidOnline: 1n });
		expect(() =>
			ConversionSession.open(
				makeReadFn(source),
				source.length,
				{ format: 'stfs', profileTransfer: {} },
				STFS_SOURCE,
			),
		).toThrow(/newGamertag\/newXuid/);
	});

	it('throws for an image-backed source (not an already-extracted STFS profile package)', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		expect(() =>
			ConversionSession.open(
				makeReadFn(iso),
				iso.length,
				{ format: 'stfs', profileTransfer: { newGamertag: 'X' } },
				XISO_SOURCE,
			),
		).toThrow(
			/profileTransfer requires an already-extracted STFS profile-package/,
		);
	});

	it('throws when the extracted STFS source has no root-level Account file', () => {
		const noAccount = makeStfsFixture({ titleId: 0x5a5a0031 }).bytes; // default.xex, not Account
		expect(() =>
			ConversionSession.open(
				makeReadFn(noAccount),
				noAccount.length,
				{ format: 'stfs', profileTransfer: { newGamertag: 'X' } },
				STFS_SOURCE,
			),
		).toThrow(/no "Account" file at package root/);
	});
});
