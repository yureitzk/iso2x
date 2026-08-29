import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import {
	makeStfsFixture,
	THUMBNAIL_SIZE_FIELD_OFFSET,
	TITLE_THUMBNAIL_SIZE_FIELD_OFFSET,
	HEADER_THUMBNAIL_MAX_SIZE_V1,
	HEADER_THUMBNAIL_MAX_SIZE_V2,
} from '../utils/fixtures/stfs.js';
import { inspectSource } from '../../dist/index.js';
import { makeReadFn, nullReadFn } from '../utils/read-fns.js';
import {
	convertXisoFixtureToGodParts,
	convertXisoFixtureToExtractedParts,
} from '../utils/session-helpers.js';
import { makeHeaderThumbnailBytes } from '../utils/fixtures/thumbnail.js';
import type { SourceRef } from '../../dist/types.js';

beforeAll(setupWasm);

const XISO_SOURCE: SourceRef = { source: { format: 'xiso' } };
const GOD_SOURCE_OPTIONS = { format: 'god' as const };
const EXTRACTED_SOURCE_OPTIONS = { format: 'extracted' as const };
const STFS_SOURCE: SourceRef = { source: { format: 'stfs' } };

// PNG signature bytes - any thumbnail decoded from the XBE path is
// re-encoded as PNG.
const PNG_SIGNATURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

// Stand-in for a real XDBF image resource: thumbnail_from_xdbf copies its
// resource bytes out verbatim, so this is expected back bit-for-bit.
const XEX_THUMB_MARKER = new TextEncoder().encode('PNG!');

/**
 * Builds real-PNG-magic header-thumbnail bytes of exactly `totalLen`
 * bytes, for exercising the reader's max-size boundary
 * (`HEADER_THUMBNAIL_MAX_SIZE_V1`/`_V2`) precisely. `makeStfsFixture`
 * always declares the size field as `bytes.length`, so the returned
 * array's length *is* the declared size - no separate poke needed.
 */
function pngOfExactSize(totalLen: number): Uint8Array {
	return makeHeaderThumbnailBytes('x'.repeat(totalLen - 8)); // 8 = PNG magic length
}

describe('inspectSource(..., includeThumbnail): Original Xbox (XBE/XPR0/DXT1) icons', () => {
	it('finds and PNG-encodes the $$XTIMAGE (title icon) for an xiso source', () => {
		const iso = makeFixture({ titleId: 0x41560001, thumbnail: {} });
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE, true);
		expect(info.thumbnail).toBeInstanceOf(Uint8Array);
		expect(Array.from((info.thumbnail as Uint8Array).slice(0, 8))).toEqual(
			PNG_SIGNATURE,
		);
	});

	it('falls back to $$XSIMAGE (savegame icon) when $$XTIMAGE is absent', () => {
		const iso = makeFixture({
			titleId: 0x41560002,
			thumbnail: { xbeSectionName: '$$XSIMAGE' },
		});
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE, true);
		expect(Array.from((info.thumbnail as Uint8Array).slice(0, 8))).toEqual(
			PNG_SIGNATURE,
		);
	});

	it('leaves thumbnail undefined when includeThumbnail is not requested, even if an icon is present', () => {
		const iso = makeFixture({ titleId: 0x41560003, thumbnail: {} });
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE);
		expect(info.thumbnail).toBeUndefined();
	});

	it('leaves thumbnail undefined when the title has no icon section at all', () => {
		const iso = makeFixture({ titleId: 0x41560004 });
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE, true);
		expect(info.thumbnail).toBeUndefined();
	});

	it('still returns the correct titleId/contentType alongside a thumbnail', () => {
		const iso = makeFixture({ titleId: 0x5a5a0007, thumbnail: {} });
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE, true);
		expect(info.titleId).toBe('5A5A0007');
		expect(info.contentType).toBe('xboxOriginal');
		expect(info.thumbnail).toBeDefined();
	});

	it('finds the same icon whether the source is xiso, god, or extracted', () => {
		const iso = makeFixture({ titleId: 0x5a5a0008, thumbnail: {} });
		const { dataParts: godParts } = convertXisoFixtureToGodParts(iso);
		const extractedParts = convertXisoFixtureToExtractedParts(iso);
		const fromXiso = inspectSource(
			makeReadFn(iso),
			iso.length,
			XISO_SOURCE,
			true,
		);
		const fromGod = inspectSource(
			nullReadFn,
			iso.length,
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
			true,
		);
		const fromExtracted = inspectSource(
			nullReadFn,
			iso.length,
			{ source: EXTRACTED_SOURCE_OPTIONS, parts: extractedParts },
			true,
		);
		expect(fromGod.thumbnail).toEqual(fromXiso.thumbnail);
		expect(fromExtracted.thumbnail).toEqual(fromXiso.thumbnail);
	});
});

describe('inspectSource(..., includeThumbnail): Xbox 360 (XEX/XDBF) icons', () => {
	it('finds the Thumb resource for an x360-platform xiso source', () => {
		const iso = makeFixture({
			titleId: 0x5a5a0001,
			platform: 'x360',
			thumbnail: {},
		});
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE, true);
		expect(info.thumbnail).toEqual(XEX_THUMB_MARKER);
	});

	it('leaves thumbnail undefined when includeThumbnail is not requested, even if a resource is present', () => {
		const iso = makeFixture({
			titleId: 0x5a5a0003,
			platform: 'x360',
			thumbnail: {},
		});
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE);
		expect(info.thumbnail).toBeUndefined();
	});

	it('leaves thumbnail undefined when the title has no thumbnail resource at all', () => {
		const iso = makeFixture({ titleId: 0x5a5a0004, platform: 'x360' });
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE, true);
		expect(info.thumbnail).toBeUndefined();
	});
	it('still returns the correct titleId/contentType alongside a thumbnail', () => {
		const iso = makeFixture({
			titleId: 0x5a5a0009,
			platform: 'x360',
			thumbnail: {},
		});
		const info = inspectSource(makeReadFn(iso), iso.length, XISO_SOURCE, true);
		expect(info.titleId).toBe('5A5A0009');
		expect(info.contentType).toBe('gamesOnDemand');
		expect(info.thumbnail).toEqual(XEX_THUMB_MARKER);
	});

	it('finds the same resource whether the source is xiso, god, or extracted', () => {
		const iso = makeFixture({
			titleId: 0x5a5a000a,
			platform: 'x360',
			thumbnail: {},
		});
		const { dataParts: godParts } = convertXisoFixtureToGodParts(iso);
		const extractedParts = convertXisoFixtureToExtractedParts(iso);
		const fromXiso = inspectSource(
			makeReadFn(iso),
			iso.length,
			XISO_SOURCE,
			true,
		);
		const fromGod = inspectSource(
			nullReadFn,
			iso.length,
			{ source: GOD_SOURCE_OPTIONS, parts: godParts },
			true,
		);
		const fromExtracted = inspectSource(
			nullReadFn,
			iso.length,
			{ source: EXTRACTED_SOURCE_OPTIONS, parts: extractedParts },
			true,
		);
		expect(fromXiso.thumbnail).toEqual(XEX_THUMB_MARKER);
		expect(fromGod.thumbnail).toEqual(fromXiso.thumbnail);
		expect(fromExtracted.thumbnail).toEqual(fromXiso.thumbnail);
	});
});

// STFS packages are Xbox 360-only, so only the XDBF icon path applies -
// there's no XBE/XPR0 path and no stfs-to-xiso/god/extracted conversion
// to cross-check against.
describe('inspectSource(..., includeThumbnail): STFS (XEX/XDBF) icons', () => {
	it('finds the Thumb resource for an stfs source', () => {
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a0001,
			thumbnail: {},
		});
		const info = inspectSource(
			makeReadFn(bytes),
			bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(info.thumbnail).toEqual(XEX_THUMB_MARKER);
	});

	it('leaves thumbnail undefined when includeThumbnail is not requested, even if a resource is present', () => {
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a0003,
			thumbnail: {},
		});
		const info = inspectSource(makeReadFn(bytes), bytes.length, STFS_SOURCE);
		expect(info.thumbnail).toBeUndefined();
	});

	it('leaves thumbnail undefined when the package has no thumbnail resource at all', () => {
		const { bytes } = makeStfsFixture({ titleId: 0x5a5a0004 });
		const info = inspectSource(
			makeReadFn(bytes),
			bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(info.thumbnail).toBeUndefined();
	});

	it('still returns the correct titleId/contentType alongside a thumbnail', () => {
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a0009,
			thumbnail: {},
		});
		const info = inspectSource(
			makeReadFn(bytes),
			bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(info.titleId).toBe('5A5A0009');
		// A plain stfs package has no per-title content_type set here, yet
		// it still resolves to Games on Demand.
		expect(info.contentType).toBe('gamesOnDemand');
		expect(info.thumbnail).toEqual(XEX_THUMB_MARKER);
	});
});

// Unlike `thumbnail` (extracted from the launch executable, with the
// header only ever a fallback), `titleThumbnail` has no executable-based
// source at all - it's read purely from the package's own header at
// 0x571A. These fixtures write real PNG-magic bytes directly into the
// header via `headerThumbnail`/`headerTitleThumbnail`, independent of the
// XEX2-embedded icon `thumbnail: {}` controls above.
describe('inspectSource(..., includeThumbnail): STFS header-embedded Thumbnail/Title Thumbnail Image', () => {
	it('reports a header-declared titleThumbnail even with no launch-executable icon', () => {
		const titleThumb = makeHeaderThumbnailBytes('TITLE');
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a000d,
			headerTitleThumbnail: titleThumb,
		});
		const info = inspectSource(
			makeReadFn(bytes),
			bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(info.titleThumbnail).toEqual(titleThumb);
	});

	it('leaves titleThumbnail undefined when includeThumbnail is not requested, even if the header field is present', () => {
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a000e,
			headerTitleThumbnail: makeHeaderThumbnailBytes('TITLE'),
		});
		const info = inspectSource(makeReadFn(bytes), bytes.length, STFS_SOURCE);
		expect(info.titleThumbnail).toBeUndefined();
	});

	it('leaves titleThumbnail undefined when the header carries no Title Thumbnail Image field', () => {
		const { bytes } = makeStfsFixture({ titleId: 0x5a5a000f });
		const info = inspectSource(
			makeReadFn(bytes),
			bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(info.titleThumbnail).toBeUndefined();
	});

	it('keeps titleThumbnail and thumbnail independent - distinct header fields round-trip separately', () => {
		const thumb = makeHeaderThumbnailBytes('THUMB');
		const titleThumb = makeHeaderThumbnailBytes('TITLE-THUMB');
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a0010,
			headerThumbnail: thumb,
			headerTitleThumbnail: titleThumb,
		});
		const info = inspectSource(
			makeReadFn(bytes),
			bytes.length,
			STFS_SOURCE,
			true,
		);
		// No XEX2 resource embedded, so `thumbnail` falls back to the
		// header's own Thumbnail Image field, same as `titleThumbnail`
		// always does.
		expect(info.thumbnail).toEqual(thumb);
		expect(info.titleThumbnail).toEqual(titleThumb);
		expect(info.thumbnail).not.toEqual(info.titleThumbnail);
	});

	it('prefers the launch-executable icon over the header Thumbnail Image for `thumbnail`, but titleThumbnail still comes from the header', () => {
		const headerThumb = makeHeaderThumbnailBytes('HEADER-FALLBACK');
		const titleThumb = makeHeaderThumbnailBytes('TITLE');
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a0011,
			thumbnail: {}, // embeds the XDBF "Thumb" resource, XEX_THUMB_MARKER bytes
			headerThumbnail: headerThumb,
			headerTitleThumbnail: titleThumb,
		});
		const info = inspectSource(
			makeReadFn(bytes),
			bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(info.thumbnail).toEqual(XEX_THUMB_MARKER);
		expect(info.thumbnail).not.toEqual(headerThumb);
		expect(info.titleThumbnail).toEqual(titleThumb);
	});
});

describe('inspectSource(..., includeThumbnail): STFS header-embedded image size validation', () => {
	it('accepts a thumbnail exactly at the Version 1 max size (0x4000 bytes)', () => {
		const thumb = pngOfExactSize(HEADER_THUMBNAIL_MAX_SIZE_V1);
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a0012,
			headerThumbnail: thumb,
		});
		const info = inspectSource(
			makeReadFn(bytes),
			bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(info.thumbnail).toEqual(thumb);
	});

	it('rejects a thumbnail one byte past the Version 1 max size', () => {
		const thumb = pngOfExactSize(HEADER_THUMBNAIL_MAX_SIZE_V1 + 1);
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a0013,
			headerThumbnail: thumb,
		});
		const info = inspectSource(
			makeReadFn(bytes),
			bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(info.thumbnail).toBeUndefined();
	});

	it('rejects header image bytes that pass the size check but aren\u2019t PNG-magic, for both thumbnail and titleThumbnail', () => {
		const notPng = new Uint8Array(32).fill(0xab); // valid size, garbage content
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a0014,
			headerThumbnail: notPng,
			headerTitleThumbnail: notPng,
		});
		const info = inspectSource(
			makeReadFn(bytes),
			bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(info.thumbnail).toBeUndefined();
		expect(info.titleThumbnail).toBeUndefined();
	});

	it('ignores real PNG bytes at the header offset when the declared size field is left at zero', () => {
		// Proves the reader gates on the declared size field, not on
		// sniffing for PNG-looking bytes at the offset: build a fixture
		// with a real embedded thumbnail (which writes both the bytes
		// and a correct size field), then zero the size field back out
		// while leaving the PNG bytes themselves untouched.
		const thumb = makeHeaderThumbnailBytes('SHOULD-BE-IGNORED');
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a0015,
			headerThumbnail: thumb,
		});
		const corrupted = new Uint8Array(bytes);
		new DataView(corrupted.buffer).setUint32(
			THUMBNAIL_SIZE_FIELD_OFFSET,
			0,
			false,
		);
		const info = inspectSource(
			makeReadFn(corrupted),
			corrupted.length,
			STFS_SOURCE,
			true,
		);
		expect(info.thumbnail).toBeUndefined();
	});

	it('ignores real PNG bytes at the Title Thumbnail header offset when its declared size field is left at zero', () => {
		// Same regression as the thumbnail case above, for the
		// independent Title Thumbnail Image size field (0x1716).
		const titleThumb = makeHeaderThumbnailBytes('SHOULD-BE-IGNORED-TOO');
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a001d,
			headerTitleThumbnail: titleThumb,
		});
		const corrupted = new Uint8Array(bytes);
		new DataView(corrupted.buffer).setUint32(
			TITLE_THUMBNAIL_SIZE_FIELD_OFFSET,
			0,
			false,
		);
		const info = inspectSource(
			makeReadFn(corrupted),
			corrupted.length,
			STFS_SOURCE,
			true,
		);
		expect(info.titleThumbnail).toBeUndefined();
	});

	it('honors both header image fields on LIVE- and PIRS-magic packages, same as CON', () => {
		for (const magic of ['LIVE', 'PIRS'] as const) {
			const thumb = makeHeaderThumbnailBytes(`${magic}-THUMB`);
			const titleThumb = makeHeaderThumbnailBytes(`${magic}-TITLE`);
			const { bytes } = makeStfsFixture({
				magic,
				titleId: 0x5a5a0016,
				headerThumbnail: thumb,
				headerTitleThumbnail: titleThumb,
			});
			const info = inspectSource(
				makeReadFn(bytes),
				bytes.length,
				STFS_SOURCE,
				true,
			);
			expect(info.thumbnail).toEqual(thumb);
			expect(info.titleThumbnail).toEqual(titleThumb);
		}
	});
});

// The Version 1 boundary above is exercised at the default
// (unwritten/0) metadataVersion. These cover Version 2's shrunk cap
// (0x3D00 instead of 0x4000) specifically - free60.org's STFS spec:
// Version 2 trims both header image fields to make room for the
// Additional Display Names/Descriptions fields it introduces.
// https://free60.org/System-Software/Formats/STFS/
describe('inspectSource(..., includeThumbnail): STFS header-embedded images at Metadata Version 2', () => {
	it('accepts a thumbnail exactly at the Version 2 max size (0x3D00 bytes)', () => {
		const thumb = pngOfExactSize(HEADER_THUMBNAIL_MAX_SIZE_V2);
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a0017,
			metadataVersion: 2,
			headerThumbnail: thumb,
		});
		const info = inspectSource(
			makeReadFn(bytes),
			bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(info.thumbnail).toEqual(thumb);
	});

	it('rejects a thumbnail one byte past the Version 2 max size', () => {
		const thumb = pngOfExactSize(HEADER_THUMBNAIL_MAX_SIZE_V2 + 1);
		const { bytes } = makeStfsFixture({
			titleId: 0x5a5a0018,
			metadataVersion: 2,
			headerThumbnail: thumb,
		});
		const info = inspectSource(
			makeReadFn(bytes),
			bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(info.thumbnail).toBeUndefined();
	});

	it('applies the same shrunk cap independently to titleThumbnail', () => {
		const accepted = pngOfExactSize(HEADER_THUMBNAIL_MAX_SIZE_V2);
		const rejected = pngOfExactSize(HEADER_THUMBNAIL_MAX_SIZE_V2 + 1);
		const { bytes: acceptedBytes } = makeStfsFixture({
			titleId: 0x5a5a0019,
			metadataVersion: 2,
			headerTitleThumbnail: accepted,
		});
		const { bytes: rejectedBytes } = makeStfsFixture({
			titleId: 0x5a5a001a,
			metadataVersion: 2,
			headerTitleThumbnail: rejected,
		});
		const acceptedInfo = inspectSource(
			makeReadFn(acceptedBytes),
			acceptedBytes.length,
			STFS_SOURCE,
			true,
		);
		const rejectedInfo = inspectSource(
			makeReadFn(rejectedBytes),
			rejectedBytes.length,
			STFS_SOURCE,
			true,
		);
		expect(acceptedInfo.titleThumbnail).toEqual(accepted);
		expect(rejectedInfo.titleThumbnail).toBeUndefined();
	});

	// The one test that actually proves the reader is *comparing against
	// a version-dependent cap* rather than just "rejects big numbers": a
	// size between the two caps must flip from accepted to rejected
	// purely based on metadataVersion, with nothing else about the
	// fixture changing.
	it('a size between the two caps is accepted at the Version 1 default but rejected at Version 2', () => {
		const midSize = HEADER_THUMBNAIL_MAX_SIZE_V2 + 0x100;
		expect(midSize).toBeLessThan(HEADER_THUMBNAIL_MAX_SIZE_V1);
		const thumb = pngOfExactSize(midSize);

		const { bytes: v1Bytes } = makeStfsFixture({
			titleId: 0x5a5a001b,
			headerThumbnail: thumb,
		});
		const v1Info = inspectSource(
			makeReadFn(v1Bytes),
			v1Bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(v1Info.thumbnail).toEqual(thumb);

		const { bytes: v2Bytes } = makeStfsFixture({
			titleId: 0x5a5a001c,
			metadataVersion: 2,
			headerThumbnail: thumb,
		});
		const v2Info = inspectSource(
			makeReadFn(v2Bytes),
			v2Bytes.length,
			STFS_SOURCE,
			true,
		);
		expect(v2Info.thumbnail).toBeUndefined();
	});
});
