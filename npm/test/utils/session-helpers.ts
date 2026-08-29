import { expect } from 'vitest';
import type {
	ConversionSession,
	OpenConversionSessionOptions,
	SourcePart,
	SourceReadFn,
} from '../../dist/index.js';
import { ConversionSession as ConversionSessionImpl } from '../../dist/index.js';
import { makeReadFn } from './read-fns.js';
import {
	stfsMinimalFixtureTimestampOffsets,
	HEADER_SIZE_FIELD_OFFSET,
	CONTENT_TYPE_FIELD_OFFSET,
	THUMBNAIL_SIZE_FIELD_OFFSET,
	TITLE_THUMBNAIL_SIZE_FIELD_OFFSET,
	THUMBNAIL_IMAGE_OFFSET,
	TITLE_THUMBNAIL_IMAGE_OFFSET,
	STFS_CONTENT_TYPE,
	STFS_WRITE_DEFAULT_HEADER_SIZE,
} from './fixtures/stfs.js';
import { writeAscii } from './fixtures/binary-utils.js';
import { XISO_SOURCE } from './sources.js';

export function driveHashing(session: ConversionSession): void {
	while (!session.hashNextPart()) {}
}

/**
 * `nextChunk`'s `chunkBytes` caps bytes per call, but every fixture here is
 * far smaller, so tests that don't care about chunk boundaries just want
 * "the whole thing in one call". i32::MAX reliably means "unbounded" here.
 *
 * Unrelated to the `0x7fffffff` bitmask used elsewhere to strip the
 * compressed-block flag from a raw ciso/cci index word - that's a 31-bit
 * position mask from the on-disk format, not a chunk size.
 */
export const UNBOUNDED_CHUNK_SIZE = 0x7fffffff;

export function driveAllChunks(
	session: ConversionSession,
	chunkBytes: number = UNBOUNDED_CHUNK_SIZE,
): Uint8Array[] {
	driveHashing(session);
	const chunks: Uint8Array[] = [];
	while (!session.isDone()) {
		const chunk = session.nextChunk(chunkBytes);
		if (chunk) chunks.push(chunk);
	}
	return chunks;
}

/** Drives a 'god' session to completion; the CON/LIVE header is the last chunk. */
export function driveToGodHeader(session: ConversionSession): Uint8Array {
	const chunks = driveAllChunks(session);
	expect(chunks.length).toBeGreaterThan(0);
	return chunks[chunks.length - 1];
}

/**
 * Drives an `stfs` session to (and returns) the header chunk. Unlike
 * `god` (header last, see `driveToGodHeader`), `stfs`'s header phase runs
 * first.
 */
export function driveToStfsHeader(session: ConversionSession): Uint8Array {
	driveHashing(session);
	return session.nextChunk(STFS_WRITE_DEFAULT_HEADER_SIZE)!;
}

export function drain(
	session: ConversionSession,
	chunkBytes: number = UNBOUNDED_CHUNK_SIZE,
): Uint8Array {
	const parts: Uint8Array[] = [];
	while (!session.isDone()) {
		const chunk = session.nextChunk(chunkBytes);
		if (chunk) parts.push(chunk);
	}
	session.free();
	return concat(parts);
}

export function driveAndDrain(
	session: ConversionSession,
	chunkBytes: number = UNBOUNDED_CHUNK_SIZE,
): Uint8Array {
	driveHashing(session);
	return drain(session, chunkBytes);
}

/**
 * Drains a session and returns its chunks grouped by `currentEntryName()`,
 * in stream order. Does not call `session.free()`; callers still own the
 * session's lifetime.
 */
export function drainNamed(
	session: ConversionSession,
	chunkBytes: number = UNBOUNDED_CHUNK_SIZE,
): Map<string, Uint8Array[]> {
	const byName = new Map<string, Uint8Array[]>();
	while (!session.isDone()) {
		const chunk = session.nextChunk(chunkBytes);
		if (!chunk) break;
		const name = session.currentEntryName();
		expect(name).not.toBeNull();
		const arr = byName.get(name as string) ?? [];
		arr.push(chunk);
		byName.set(name as string, arr);
	}
	return byName;
}

export function concat(chunks: Uint8Array[]): Uint8Array {
	const total = chunks.reduce((n, c) => n + c.length, 0);
	const out = new Uint8Array(total);
	let pos = 0;
	for (const c of chunks) {
		out.set(c, pos);
		pos += c.length;
	}
	return out;
}

/**
 * Drives a `ConversionSession` to completion and returns its single output
 * file's bytes, for building small ciso/cci fixtures from the existing
 * xiso fixture builder instead of hand-rolling either format's layout.
 *
 * Only call this inside `beforeAll`/`it` callbacks, never at the top level
 * of a `describe` body - vitest runs `describe` bodies synchronously
 * during collection, so calling into the wasm session there crashes it
 * before any test has started.
 *
 * Asserts exactly one output part is produced - callers' fixtures are far
 * too small to cross a real split point, so a failure here means this
 * assumption needs revisiting.
 */
export function convertXisoFixtureToBytes(
	xiso: Uint8Array,
	options: OpenConversionSessionOptions,
): Uint8Array {
	const session = ConversionSessionImpl.open(
		makeReadFn(xiso),
		xiso.length,
		options,
		XISO_SOURCE,
	);
	try {
		while (!session.hashNextPart()) {
			/* keep driving the sizing pass */
		}
		const chunks: Uint8Array[] = [];
		let total = 0;
		while (!session.isDone()) {
			const chunk = session.nextChunk(1024 * 1024);
			if (chunk === null) break;
			chunks.push(chunk);
			total += chunk.length;
		}
		expect(session.outputManifest().length).toBe(1);
		const out = new Uint8Array(total);
		let offset = 0;
		for (const chunk of chunks) {
			out.set(chunk, offset);
			offset += chunk.length;
		}
		return out;
	} finally {
		session.free();
	}
}

export type GodPart = { name: string; size: number; readFn: SourceReadFn };

/**
 * GOD counterpart to `convertXisoFixtureToBytes`. Drives a `'god'` session
 * to completion and slices the chunk stream by each `outputManifest()`
 * entry's `size`, in listed order (header last) - `currentEntryName()` is
 * always `null` for `'god'` sessions, so this is the only way to recover
 * per-file bytes.
 *
 * Returns the `Data%04d` parts and the trailing CON/LIVE header entry
 * separately; `headerPart` is for tests that want to feed the real header
 * back in too (e.g. GodSource's content-type override).
 */
export function convertXisoFixtureToGodParts(
	xiso: Uint8Array,
	options: Extract<OpenConversionSessionOptions, { format: 'god' }> = {
		format: 'god',
	},
): { dataParts: GodPart[]; headerPart: GodPart } {
	const session = ConversionSessionImpl.open(
		makeReadFn(xiso),
		xiso.length,
		options,
		XISO_SOURCE,
	);
	try {
		while (!session.hashNextPart()) {
			/* keep driving the sizing pass */
		}
		const manifest = session.outputManifest();
		expect(manifest.length).toBeGreaterThanOrEqual(2); // Data part(s) + header
		const chunks: Uint8Array[] = [];
		let total = 0;
		while (!session.isDone()) {
			const chunk = session.nextChunk(1024 * 1024);
			if (chunk === null) break;
			chunks.push(chunk);
			total += chunk.length;
		}
		const all = new Uint8Array(total);
		let offset = 0;
		for (const chunk of chunks) {
			all.set(chunk, offset);
			offset += chunk.length;
		}
		let cursor = 0;
		const parts: GodPart[] = manifest.map(({ name, size }) => {
			const bytes = all.slice(cursor, cursor + size);
			cursor += size;
			return { name, size: bytes.length, readFn: makeReadFn(bytes) };
		});
		return {
			dataParts: parts.slice(0, -1),
			headerPart: parts[parts.length - 1],
		};
	} finally {
		session.free();
	}
}

/**
 * Builds a synthetic CON-magic header `SourcePart` by hand instead of
 * driving a real signed write session. A GOD source only reads the fixed
 * magic/content-type prefix and, optionally, the Thumbnail/Title
 * Thumbnail Image fields (0x171A/0x571A) off a header part, so that's
 * enough to exercise the override fields without a signing key or icon.
 *
 * `name` only needs to not match a `Data%04d` part for the JS-side split
 * to treat it as the header.
 */
export function makeSyntheticGodHeaderPart(
	opts: {
		name?: string;
		contentType?: number;
		thumbnail?: Uint8Array;
		titleThumbnail?: Uint8Array;
	} = {},
): GodPart {
	const buf = new Uint8Array(TITLE_THUMBNAIL_IMAGE_OFFSET + 0x4000);
	const view = new DataView(buf.buffer);
	writeAscii(buf, 0, 'CON ');
	view.setUint32(HEADER_SIZE_FIELD_OFFSET, 0x1000, false);
	view.setUint32(
		CONTENT_TYPE_FIELD_OFFSET,
		opts.contentType ?? STFS_CONTENT_TYPE.installedGame,
		false,
	);
	if (opts.thumbnail) {
		view.setUint32(THUMBNAIL_SIZE_FIELD_OFFSET, opts.thumbnail.length, false);
		buf.set(opts.thumbnail, THUMBNAIL_IMAGE_OFFSET);
	}
	if (opts.titleThumbnail) {
		view.setUint32(
			TITLE_THUMBNAIL_SIZE_FIELD_OFFSET,
			opts.titleThumbnail.length,
			false,
		);
		buf.set(opts.titleThumbnail, TITLE_THUMBNAIL_IMAGE_OFFSET);
	}
	return {
		name: opts.name ?? 'synthetic-header.bin',
		size: buf.length,
		readFn: makeReadFn(buf),
	};
}

/**
 * Extracted counterpart to `convertXisoFixtureToGodParts`: no fixture
 * builder emits a flat `{ name, size, readFn }[]` of loose files
 * directly, so this reuses the `format: 'extracted'` target path to
 * produce that shape from the xiso fixture builder.
 *
 * Slices the chunk stream by each manifest entry's `size`, same technique
 * as `convertXisoFixtureToGodParts`.
 *
 * Unlike that function, no `hashNextPart()` driving is needed first:
 * `'extracted'` sessions no-op that and the manifest is available
 * immediately after `open()`.
 */
export function convertXisoFixtureToExtractedParts(
	xiso: Uint8Array,
	options: Extract<OpenConversionSessionOptions, { format: 'extracted' }> = {
		format: 'extracted',
	},
): { name: string; size: number; readFn: SourceReadFn }[] {
	const session = ConversionSessionImpl.open(
		makeReadFn(xiso),
		xiso.length,
		options,
		XISO_SOURCE,
	);
	try {
		const manifest = session.outputManifest();
		expect(manifest.length).toBeGreaterThanOrEqual(1);
		const chunks: Uint8Array[] = [];
		let total = 0;
		while (!session.isDone()) {
			const chunk = session.nextChunk(1024 * 1024);
			if (chunk === null) break;
			chunks.push(chunk);
			total += chunk.length;
		}
		const all = new Uint8Array(total);
		let offset = 0;
		for (const chunk of chunks) {
			all.set(chunk, offset);
			offset += chunk.length;
		}
		let cursor = 0;
		return manifest.map(({ name, size }) => {
			const bytes = all.slice(cursor, cursor + size);
			cursor += size;
			return { name, size: bytes.length, readFn: makeReadFn(bytes) };
		});
	} finally {
		session.free();
	}
}

/**
 * Patches the big-endian `TitleExecutionInfo` struct in an x360 (XEX2)
 * fixture built via `makeFixture({ platform: 'x360' })`, to set
 * mediaId/discNumber/discCount without rebuilding the whole fixture.
 * `0x22 * 0x800 + 0x30` mirrors the stub-executable sector and
 * TitleExecutionInfo offset documented in xsf.ts. `rootOffset` shifts
 * that base for fixtures embedded at a non-zero offset.
 *
 * Only meaningful for `platform: 'x360'` fixtures - OGX (XBE) has no
 * equivalent fields.
 */
export function patchXexExecutionInfo(
	xex: Uint8Array,
	fields: { mediaId?: number; discNumber?: number; discCount?: number },
	rootOffset = 0,
): Uint8Array {
	const patched = xex.slice();
	const view = new DataView(patched.buffer, patched.byteOffset, patched.length);
	const INFO = rootOffset + 0x22 * 0x800 + 0x30;
	if (fields.mediaId !== undefined) {
		view.setUint32(INFO + 0x00, fields.mediaId, false);
	}
	if (fields.discNumber !== undefined) {
		patched[INFO + 0x12] = fields.discNumber;
	}
	if (fields.discCount !== undefined) {
		patched[INFO + 0x13] = fields.discCount;
	}
	return patched;
}

// Multi-part source construction (cci/ciso splitting tests), shared here
// so any future split-capable source format can reuse it.

/** Wraps bytes as a named SourcePart. */
export function makePart(name: string, bytes: Uint8Array): SourcePart {
	return { name, size: bytes.length, readFn: makeReadFn(bytes) };
}

// CCI splitting

// HEADER_SIZE isn't exposed via wasm - 32 mirrors CCI's fixed header size.
const CCI_HEADER_SIZE = 32;
const CCI_MAGIC = new Uint8Array([0x43, 0x43, 0x49, 0x4d]); // "CCIM"

/** Parsed view of a single ".N.cci" file's header + index table. */
export interface CciLayout {
	blockSize: number;
	version: number;
	align: number;
	totalSectors: number;
	indexOffset: number;
	// One raw little-endian u32 index word per entry (sector count + 1,
	// the last being the sentinel index_end with no compressed bit).
	rawIndexWords: number[];
	// Byte offset of each sector slot (index i) plus the trailing
	// sentinel offset at index totalSectors, decoded from rawIndexWords.
	positions: number[];
}

export function parseCciLayout(bytes: Uint8Array): CciLayout {
	const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.length);
	const uncompressedSize = view.getBigUint64(8, true);
	const indexOffset = Number(view.getBigUint64(16, true));
	const blockSize = view.getUint32(24, true);
	const version = view.getUint8(28);
	const align = view.getUint8(29);
	const totalSectors = Number(uncompressedSize / BigInt(blockSize));

	const rawIndexWords: number[] = [];
	const positions: number[] = [];
	for (let i = 0; i <= totalSectors; i++) {
		const raw = view.getUint32(indexOffset + i * 4, true);
		rawIndexWords.push(raw);
		positions.push((raw & 0x7fffffff) << align);
	}

	return {
		blockSize,
		version,
		align,
		totalSectors,
		indexOffset,
		rawIndexWords,
		positions,
	};
}

function buildCciHeader(
	uncompressedSize: number,
	indexOffset: number,
	layout: CciLayout,
): Uint8Array {
	const header = new Uint8Array(CCI_HEADER_SIZE);
	const view = new DataView(header.buffer);
	header.set(CCI_MAGIC, 0);
	view.setUint32(4, CCI_HEADER_SIZE, true);
	view.setBigUint64(8, BigInt(uncompressedSize), true);
	view.setBigUint64(16, BigInt(indexOffset), true);
	view.setUint32(24, layout.blockSize, true);
	view.setUint8(28, layout.version);
	view.setUint8(29, layout.align);
	view.setUint16(30, 0, true);
	return header;
}

function wordsToBytes(words: number[]): Uint8Array {
	const out = new Uint8Array(words.length * 4);
	const view = new DataView(out.buffer);
	words.forEach((w, i) => view.setUint32(i * 4, w >>> 0, true));
	return out;
}

function concatBytes(...chunks: Uint8Array[]): Uint8Array {
	const out = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
	let offset = 0;
	for (const chunk of chunks) {
		out.set(chunk, offset);
		offset += chunk.length;
	}
	return out;
}

/**
 * Splits a valid, single-part ".1.cci" file into two self-contained parts
 * at `splitSector`, the same way a real split write would. Avoids needing
 * a multi-GB fixture to exercise a real split.
 */
export function splitCciAt(
	bytes: Uint8Array,
	splitSector: number,
): { part1: Uint8Array; part2: Uint8Array } {
	const layout = parseCciLayout(bytes);
	if (splitSector <= 0 || splitSector >= layout.totalSectors) {
		throw new Error(
			`splitCciAt: splitSector ${splitSector} out of range (0, ${layout.totalSectors})`,
		);
	}
	const splitBytePos = layout.positions[splitSector];

	// Part 1: prefix sectors, already relative to the shared HEADER_SIZE
	// start, so the index words are reused verbatim; only the header and
	// trailing sentinel change.
	const sectorBytes1 = bytes.slice(CCI_HEADER_SIZE, splitBytePos);
	const indexWords1 = [
		...layout.rawIndexWords.slice(0, splitSector),
		splitBytePos >> layout.align,
	];
	const header1 = buildCciHeader(
		splitSector * layout.blockSize,
		splitBytePos,
		layout,
	);

	// Part 2: suffix sectors, rebased to start right after its own header.
	const sectorBytes2 = bytes.slice(splitBytePos, layout.indexOffset);
	const rebase = (origPos: number) => CCI_HEADER_SIZE + (origPos - splitBytePos);
	const part2Count = layout.totalSectors - splitSector;
	const indexWords2 = Array.from({ length: part2Count }, (_, i) => {
		const compressedBit = layout.rawIndexWords[splitSector + i] & 0x80000000;
		return (
			((rebase(layout.positions[splitSector + i]) >> layout.align) |
				compressedBit) >>>
			0
		);
	});
	const sentinelPos2 = rebase(layout.positions[layout.totalSectors]);
	indexWords2.push(sentinelPos2 >> layout.align);
	const header2 = buildCciHeader(
		part2Count * layout.blockSize,
		sentinelPos2,
		layout,
	);

	return {
		part1: concatBytes(header1, sectorBytes1, wordsToBytes(indexWords1)),
		part2: concatBytes(header2, sectorBytes2, wordsToBytes(indexWords2)),
	};
}

// CISO splitting

/**
 * Cuts a real, valid single-part `.cso` into two files at `splitSector`,
 * patching the index table to match a genuine split, without needing a
 * multi-GB fixture to cross FILE_SPLIT_POINT.
 */
export function splitCisoAt(
	bytes: Uint8Array,
	splitSector: number,
): { part1: Uint8Array; part2: Uint8Array } {
	const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.length);
	const uncompressedSize = view.getBigUint64(8, true);
	const blockSize = view.getUint32(16, true);
	const align = view.getUint8(21);
	const totalDataSectors = Number(uncompressedSize / BigInt(blockSize));
	if (splitSector <= 0 || splitSector >= totalDataSectors) {
		throw new Error(
			`splitCisoAt: splitSector ${splitSector} out of range (0, ${totalDataSectors})`,
		);
	}

	const indexTableOffset = 24;
	const entryOffset = (i: number) => indexTableOffset + i * 4;
	const rawPosition = (i: number) => view.getUint32(entryOffset(i), true);
	// Positions are shift-encoded (position << align) with the compression
	// flag in the top bit.
	const shiftedPos = (raw: number) => raw & 0x7fffffff;
	const compressionFlag = (raw: number) => raw & 0x80000000;

	const splitBytePos = shiftedPos(rawPosition(splitSector)) << align;

	const part1 = bytes.slice(0, splitBytePos);
	const part2 = bytes.slice(splitBytePos);

	// Rewrite every entry from splitSector through the trailing sentinel to
	// be relative to part2's own start, matching what the real writer does
	// when it resets write_pos to 0 for a new physical part.
	const part1View = new DataView(part1.buffer, part1.byteOffset, part1.length);
	for (let i = splitSector; i <= totalDataSectors; i++) {
		const raw = rawPosition(i);
		const oldBytePos = shiftedPos(raw) << align;
		const newBytePos = oldBytePos - splitBytePos;
		const newShifted = newBytePos >> align;
		part1View.setUint32(
			entryOffset(i),
			(newShifted & 0x7fffffff) | compressionFlag(raw),
			true,
		);
	}

	return { part1, part2 };
}

/**
 * Asserts `a`/`b` (two drained STFS write-session outputs from
 * independent `open()` calls over identical input) are byte-identical
 * except the embedded timestamp windows - see
 * `stfsMinimalFixtureTimestampOffsets` for why those may differ.
 */
export function expectStfsOutputDeterministicIgnoringTimestamps(
	a: Uint8Array,
	b: Uint8Array,
): void {
	expect(a.length).toBe(b.length);
	const { createdTimestamp, accessTimestamp } =
		stfsMinimalFixtureTimestampOffsets();
	expect(accessTimestamp + 4).toBeLessThanOrEqual(a.length);
	const inTimestampWindow = (i: number): boolean =>
		(i >= createdTimestamp && i < createdTimestamp + 4) ||
		(i >= accessTimestamp && i < accessTimestamp + 4);
	for (let i = 0; i < a.length; i++) {
		if (a[i] !== b[i] && !inTimestampWindow(i)) {
			throw new Error(
				`byte ${i} differs outside the known timestamp windows ` +
					`(createdTimeStamp @ ${createdTimestamp}, accessTimeStamp @ ` +
					`${accessTimestamp}): ${a[i]} vs ${b[i]}`,
			);
		}
	}
}
