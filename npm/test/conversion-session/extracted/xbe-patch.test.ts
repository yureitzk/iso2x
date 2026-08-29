import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../../utils/wasm-setup.js';
import {
	makeFixture,
	DEFAULT_XBE_DECLARED_SIZE,
} from '../../utils/fixtures/xsf.js';
import { ConversionSession } from '../../../dist/index.js';
import { makeReadFn } from '../../utils/read-fns.js';
import { UNBOUNDED_CHUNK_SIZE } from '../../utils/session-helpers.js';
import { readU32LE, certAddr } from '../../utils/fixtures/binary-utils.js';
import { XISO_SOURCE } from '../../utils/sources.js';

beforeAll(setupWasm);

// The default fixture's default.xbe is DEFAULT_XBE_DECLARED_SIZE (0x400),
// which comfortably covers the certificate's 0x200..0x3D0 range. Triggering
// the "certificate is out of bounds" error path below needs an explicit,
// deliberately undersized value instead - smaller than 0x3D0 (976).
const TOO_SMALL_XBE_SIZE = 0x300;

const CERT_ALLOWED_MEDIA_OFFSET = 0x9c;
const CERT_TITLE_NAME_OFFSET = 0x0c;
const CERT_TITLE_NAME_LEN = 80;
const HARD_DISK = 0x00000001;
const MEDIA_BOARD = 0x00000200;
const NONSECURE_HARD_DISK = 0x40000000;

describe('ConversionSession(extracted) xbe patch options - no default.xbe at root', () => {
	// x360 fixtures ship default.xex, not default.xbe, so xbe_index resolves
	// to None and the patch step is a documented no-op "regardless of
	// xbe_patch" (see ExtractedSession::open's doc comment). This is one
	// scenario the standard fixture shape can exercise end-to-end without
	// any xbeDeclaredSize override, since it never reaches
	// patch_xbe_cert_in_place at all.
	it('allowedMediaPatch does not throw and does not alter output when the source has no default.xbe', () => {
		const iso = makeFixture({ titleId: 0x5a5a0001, platform: 'x360' });
		const readFn = makeReadFn(iso);

		const unpatched = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'extracted' },
			XISO_SOURCE,
		);
		const unpatchedChunk = unpatched.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		unpatched.free();

		const patched = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'extracted', allowedMediaPatch: true, renameTitle: 'Renamed' },
			XISO_SOURCE,
		);
		expect(() => patched.nextChunk(UNBOUNDED_CHUNK_SIZE)).not.toThrow();
		patched.free();

		const patchedAgain = ConversionSession.open(
			readFn,
			iso.length,
			{ format: 'extracted', allowedMediaPatch: true, renameTitle: 'Renamed' },
			XISO_SOURCE,
		);
		const patchedChunk = patchedAgain.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		patchedAgain.free();
		expect(patchedChunk).toEqual(unpatchedChunk);
	});

	it('currentEntryName still reports default.xex, unaffected by xbe_patch being set', () => {
		const iso = makeFixture({ titleId: 0x5a5a0002, platform: 'x360' });
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'extracted', allowedMediaPatch: true },
			XISO_SOURCE,
		);
		session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		expect(session.currentEntryName()).toBe('default.xex');
		session.free();
	});
});

describe('ConversionSession(extracted) xbe patch options - default.xbe too small (explicit undersized fixture)', () => {
	// patch_xbe_cert_in_place needs the buffer to reach 0x3D0 (976) bytes -
	// the cert starts at offset 0x200 and is 464 (0x1D0) bytes wide.
	// ExtractedSession reads exactly `entry.size` bytes before patching
	// (see next_chunk's `vec![0u8; entry.size as usize]`), so against an
	// undersized fixture this fails loudly rather than silently patching a
	// truncated/wrong certificate - confirmed against the real wasm build,
	// not just predicted from reading the source. The "default.xbe
	// present, big enough" block below covers the success path.
	it('allowedMediaPatch throws because the fixture default.xbe is smaller than a full certificate', () => {
		const iso = makeFixture({
			titleId: 0x41560001,
			xbeDeclaredSize: TOO_SMALL_XBE_SIZE,
		});
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'extracted', allowedMediaPatch: true },
			XISO_SOURCE,
		);
		expect(() => session.nextChunk(UNBOUNDED_CHUNK_SIZE)).toThrow(
			/certificate is out of bounds/,
		);
		session.free();
	});

	it('renameTitle throws for the same reason', () => {
		const iso = makeFixture({
			titleId: 0x41560001,
			xbeDeclaredSize: TOO_SMALL_XBE_SIZE,
		});
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'extracted', renameTitle: 'My Game' },
			XISO_SOURCE,
		);
		expect(() => session.nextChunk(UNBOUNDED_CHUNK_SIZE)).toThrow(
			/certificate is out of bounds/,
		);
		session.free();
	});

	it('without allowedMediaPatch/renameTitle, the same fixture opens and drains fine', () => {
		// Confirms the failures above are specifically about the patch path,
		// not about this fixture/entry size being broken in general.
		const iso = makeFixture({
			titleId: 0x41560001,
			xbeDeclaredSize: TOO_SMALL_XBE_SIZE,
		});
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'extracted' },
			XISO_SOURCE,
		);
		expect(() => session.nextChunk(UNBOUNDED_CHUNK_SIZE)).not.toThrow();
		session.free();
	});
});

describe('ConversionSession(extracted) xbe patch options - default.xbe present, big enough', () => {
	// The fixture's default.xbe is already DEFAULT_XBE_DECLARED_SIZE by
	// default, so passing xbeDeclaredSize here is redundant - kept
	// explicit anyway so it's obvious at each call site that these tests
	// specifically depend on the fixture being big enough for a full
	// certificate, unlike the "too small" block above.
	it('allowedMediaPatch ORs in the expected allowed_media_types bits', () => {
		const iso = makeFixture({
			titleId: 0x41560001,
			xbeDeclaredSize: DEFAULT_XBE_DECLARED_SIZE,
		});
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'extracted', allowedMediaPatch: true },
			XISO_SOURCE,
		);
		const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		session.free();

		const addr = certAddr(chunk);
		const allowedMedia = readU32LE(chunk, addr + CERT_ALLOWED_MEDIA_OFFSET);
		expect(allowedMedia & (HARD_DISK | MEDIA_BOARD | NONSECURE_HARD_DISK)).toBe(
			HARD_DISK | MEDIA_BOARD | NONSECURE_HARD_DISK,
		);
	});

	it('renameTitle overwrites title_name, UTF-16LE-encoded', () => {
		const iso = makeFixture({
			titleId: 0x41560001,
			xbeDeclaredSize: DEFAULT_XBE_DECLARED_SIZE,
		});
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'extracted', renameTitle: 'My Game' },
			XISO_SOURCE,
		);
		const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		session.free();

		const addr = certAddr(chunk);
		const nameBytes = chunk.slice(
			addr + CERT_TITLE_NAME_OFFSET,
			addr + CERT_TITLE_NAME_OFFSET + CERT_TITLE_NAME_LEN,
		);
		const decoded = new TextDecoder('utf-16le')
			.decode(nameBytes)
			.replace(/\0+$/, '');
		expect(decoded).toBe('My Game');
	});

	it('allowedMediaPatch without renameTitle leaves title_name untouched (all zero, matching the fixture source)', () => {
		const iso = makeFixture({
			titleId: 0x41560001,
			xbeDeclaredSize: DEFAULT_XBE_DECLARED_SIZE,
		});
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'extracted', allowedMediaPatch: true },
			XISO_SOURCE,
		);
		const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		session.free();

		const addr = certAddr(chunk);
		const nameBytes = chunk.slice(
			addr + CERT_TITLE_NAME_OFFSET,
			addr + CERT_TITLE_NAME_OFFSET + CERT_TITLE_NAME_LEN,
		);
		expect(nameBytes.every((b) => b === 0)).toBe(true);
	});

	it('both options together patch independently in one pass', () => {
		const iso = makeFixture({
			titleId: 0x41560001,
			xbeDeclaredSize: DEFAULT_XBE_DECLARED_SIZE,
		});
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{
				format: 'extracted',
				allowedMediaPatch: true,
				renameTitle: 'Combo',
			},
			XISO_SOURCE,
		);
		const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		session.free();

		const addr = certAddr(chunk);
		const allowedMedia = readU32LE(chunk, addr + CERT_ALLOWED_MEDIA_OFFSET);
		expect(allowedMedia & (HARD_DISK | MEDIA_BOARD | NONSECURE_HARD_DISK)).toBe(
			HARD_DISK | MEDIA_BOARD | NONSECURE_HARD_DISK,
		);
		const nameBytes = chunk.slice(
			addr + CERT_TITLE_NAME_OFFSET,
			addr + CERT_TITLE_NAME_OFFSET + CERT_TITLE_NAME_LEN,
		);
		const decoded = new TextDecoder('utf-16le')
			.decode(nameBytes)
			.replace(/\0+$/, '');
		expect(decoded).toBe('Combo');
	});

	it('currentEntryName still reports default.xbe when patched', () => {
		const iso = makeFixture({
			titleId: 0x41560001,
			xbeDeclaredSize: DEFAULT_XBE_DECLARED_SIZE,
		});
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'extracted', allowedMediaPatch: true },
			XISO_SOURCE,
		);
		session.nextChunk(UNBOUNDED_CHUNK_SIZE);
		expect(session.currentEntryName()).toBe('default.xbe');
		session.free();
	});

	it('patched output size still matches the declared entry size', () => {
		const iso = makeFixture({
			titleId: 0x41560001,
			xbeDeclaredSize: DEFAULT_XBE_DECLARED_SIZE,
		});
		const session = ConversionSession.open(
			makeReadFn(iso),
			iso.length,
			{ format: 'extracted', allowedMediaPatch: true, renameTitle: 'X' },
			XISO_SOURCE,
		);
		const chunk = session.nextChunk(UNBOUNDED_CHUNK_SIZE)!;
		session.free();
		expect(chunk.length).toBe(DEFAULT_XBE_DECLARED_SIZE);
	});
});
