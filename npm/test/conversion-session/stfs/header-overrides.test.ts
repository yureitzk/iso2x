import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import { makeFixture } from '../../utils/fixtures/xsf.js';
import {
	makeStfsFixture,
	STFS_WRITE_CONSOLE_ID_OFFSET as CONSOLE_ID_OFFSET,
	STFS_WRITE_CONSOLE_ID_LEN as CONSOLE_ID_LEN,
	STFS_WRITE_PROFILE_ID_OFFSET as PROFILE_ID_OFFSET,
	STFS_WRITE_PROFILE_ID_LEN as PROFILE_ID_LEN,
	STFS_WRITE_ONLINE_CREATOR_OFFSET as ONLINE_CREATOR_OFFSET,
	STFS_WRITE_ONLINE_CREATOR_LEN as ONLINE_CREATOR_LEN,
	STFS_WRITE_DEVICE_ID_OFFSET as DEVICE_ID_OFFSET,
	STFS_WRITE_DEVICE_ID_LEN as DEVICE_ID_LEN,
} from '../../utils/fixtures/stfs.js';
import { ConversionSession } from '../../../dist/index.js';
import { makeReadFn } from '../../utils/read-fns.js';
import { driveToStfsHeader as driveToHeader } from '../../utils/session-helpers.js';
import { STFS_SOURCE, XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

function bytesAt(header: Uint8Array, offset: number, len: number): Uint8Array {
	return header.slice(offset, offset + len);
}

/** Deterministic filler, distinguishable per call site so a wrong offset
 * shows up as a mismatch rather than an accidental match against another
 * field's bytes. */
function fill(len: number, start: number): Uint8Array {
	return Uint8Array.from({ length: len }, (_, i) => (start + i) & 0xff);
}

/**
 * Patches one of the four identity fields directly into a raw STFS
 * fixture buffer's header, for "preserve from source" round-trip tests.
 * `makeStfsFixture` itself has no options for these display-only
 * fields, and the reader reads them unconditionally regardless of
 * package validity/signing, so writing them in after the fact is
 * sufficient - no need to extend the fixture builder itself.
 */
function patchField(
	bytes: Uint8Array,
	offset: number,
	value: Uint8Array,
): Uint8Array {
	const out = bytes.slice();
	out.set(value, offset);
	return out;
}

describe('ConversionSession(stfs) header identity field overrides', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);

	it('writes an explicit consoleId override at 0x36C (5 bytes)', () => {
		const consoleId = fill(CONSOLE_ID_LEN, 0x01);
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs', consoleId },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(bytesAt(header, CONSOLE_ID_OFFSET, CONSOLE_ID_LEN)).toEqual(consoleId);
	});

	it('writes an explicit profileId override at 0x371 (8 bytes)', () => {
		const profileId = fill(PROFILE_ID_LEN, 0x10);
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs', profileId },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(bytesAt(header, PROFILE_ID_OFFSET, PROFILE_ID_LEN)).toEqual(profileId);
	});

	it('writes an explicit deviceId override at 0x3FD (20 bytes)', () => {
		const deviceId = fill(DEVICE_ID_LEN, 0x20);
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs', deviceId },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(bytesAt(header, DEVICE_ID_OFFSET, DEVICE_ID_LEN)).toEqual(deviceId);
	});

	it('writes an explicit onlineCreator override at 0x3AD (8 bytes), distinct from profileId', () => {
		const profileId = fill(PROFILE_ID_LEN, 0x30);
		const onlineCreator = fill(ONLINE_CREATOR_LEN, 0x40);
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs', profileId, onlineCreator },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(bytesAt(header, ONLINE_CREATOR_OFFSET, ONLINE_CREATOR_LEN)).toEqual(
			onlineCreator,
		);
		expect(bytesAt(header, PROFILE_ID_OFFSET, PROFILE_ID_LEN)).toEqual(profileId);
	});

	it('all four fields default to zero for a non-stfs source with no overrides', () => {
		const session = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'stfs' },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		for (const [offset, len] of [
			[CONSOLE_ID_OFFSET, CONSOLE_ID_LEN],
			[PROFILE_ID_OFFSET, PROFILE_ID_LEN],
			[ONLINE_CREATOR_OFFSET, ONLINE_CREATOR_LEN],
			[DEVICE_ID_OFFSET, DEVICE_ID_LEN],
		] as const) {
			expect(bytesAt(header, offset, len).every((b) => b === 0)).toBe(true);
		}
	});
});

describe('ConversionSession(stfs) header identity fields on an stfs->stfs round trip', () => {
	it('preserves the source header\u2019s consoleId/profileId/deviceId/onlineCreator when no override is given', () => {
		const consoleId = fill(CONSOLE_ID_LEN, 0x51);
		const profileId = fill(PROFILE_ID_LEN, 0x52);
		const deviceId = fill(DEVICE_ID_LEN, 0x53);
		const onlineCreator = fill(ONLINE_CREATOR_LEN, 0x54);
		let source = makeStfsFixture({ titleId: 0x5a5a0020 }).bytes;
		source = patchField(source, CONSOLE_ID_OFFSET, consoleId);
		source = patchField(source, PROFILE_ID_OFFSET, profileId);
		source = patchField(source, DEVICE_ID_OFFSET, deviceId);
		source = patchField(source, ONLINE_CREATOR_OFFSET, onlineCreator);
		const session = ConversionSession.open(
			makeReadFn(source),
			source.length,
			{ format: 'stfs' },
			STFS_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(bytesAt(header, CONSOLE_ID_OFFSET, CONSOLE_ID_LEN)).toEqual(consoleId);
		expect(bytesAt(header, PROFILE_ID_OFFSET, PROFILE_ID_LEN)).toEqual(profileId);
		expect(bytesAt(header, DEVICE_ID_OFFSET, DEVICE_ID_LEN)).toEqual(deviceId);
		expect(bytesAt(header, ONLINE_CREATOR_OFFSET, ONLINE_CREATOR_LEN)).toEqual(
			onlineCreator,
		);
	});

	it('an explicit override wins over the source header\u2019s own value', () => {
		const sourceProfileId = fill(PROFILE_ID_LEN, 0x60);
		const overrideProfileId = fill(PROFILE_ID_LEN, 0x70);
		const source = patchField(
			makeStfsFixture({ titleId: 0x5a5a0021 }).bytes,
			PROFILE_ID_OFFSET,
			sourceProfileId,
		);
		const session = ConversionSession.open(
			makeReadFn(source),
			source.length,
			{ format: 'stfs', profileId: overrideProfileId },
			STFS_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(bytesAt(header, PROFILE_ID_OFFSET, PROFILE_ID_LEN)).toEqual(
			overrideProfileId,
		);
	});

	it('an image-backed (non-stfs) source falls back to zero rather than preserving anything', () => {
		// Guards against a regression where "preserve from source" reads
		// stale/uninitialized bytes instead of correctly falling back to
		// zero for an image-backed (not stfs-backed) source.
		const iso = makeFixture({ titleId: 0x5a5a0022 });
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'stfs' },
			XISO_SOURCE,
		);
		const header = driveToHeader(session);
		session.free();
		expect(
			bytesAt(header, DEVICE_ID_OFFSET, DEVICE_ID_LEN).every((b) => b === 0),
		).toBe(true);
	});
});

describe('ConversionSession(stfs) header identity field overrides: validation', () => {
	const iso = makeFixture({ titleId: 0x41560001 });
	const readFn = makeReadFn(iso);

	it('rejects a consoleId that is not exactly 5 bytes', () => {
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'stfs', consoleId: new Uint8Array(4) },
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('rejects a profileId that is not exactly 8 bytes', () => {
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'stfs', profileId: new Uint8Array(7) },
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('rejects a deviceId that is not exactly 20 bytes', () => {
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'stfs', deviceId: new Uint8Array(19) },
				XISO_SOURCE,
			),
		).toThrow();
	});

	it('rejects an onlineCreator that is not exactly 8 bytes, with the exact wasm-side message', () => {
		expect(() =>
			ConversionSession.open(
				readFn,
				iso.length,
				{ format: 'stfs', onlineCreator: new Uint8Array(9) },
				XISO_SOURCE,
			),
		).toThrow(/onlineCreator must be exactly 8 bytes, got 9/);
	});
});
