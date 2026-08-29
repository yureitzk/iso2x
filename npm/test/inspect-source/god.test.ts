import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { inspectSource } from '../../dist/index.js';
import { makeReadFn, nullReadFn, throwingReadFn } from '../utils/read-fns.js';
import {
	convertXisoFixtureToGodParts,
	makeSyntheticGodHeaderPart,
} from '../utils/session-helpers.js';
import { makeSyntheticKeyvault } from '../utils/fixtures/keyvault.js';
import { makeHeaderThumbnailBytes } from '../utils/fixtures/thumbnail.js';
import {
	GOD_SOURCE_OPTIONS as GOD_SOURCE,
	XISO_SOURCE_OPTIONS,
} from '../utils/sources.js';

beforeAll(setupWasm);

describe('inspectSource with a `god` source', () => {
	let iso: Uint8Array;
	let godParts: ReturnType<typeof convertXisoFixtureToGodParts>['dataParts'];

	beforeAll(() => {
		iso = makeFixture({ titleId: 0x6a6a0003, version: 2 });
		({ dataParts: godParts } = convertXisoFixtureToGodParts(iso));
	});

	it('produces at least one Data%04d part, named in part order', () => {
		expect(godParts.length).toBeGreaterThanOrEqual(1);
		expect(godParts[0].name).toMatch(/\.data\/Data0000$/);
	});

	it('parses without throwing', () => {
		expect(() =>
			inspectSource(nullReadFn, iso.length, {
				source: GOD_SOURCE,
				parts: godParts,
			}),
		).not.toThrow();
	});

	it('returns the same titleId/contentType as the original xiso fixture', () => {
		const original = inspectSource(makeReadFn(iso), iso.length, {
			source: XISO_SOURCE_OPTIONS,
		});
		const converted = inspectSource(nullReadFn, iso.length, {
			source: GOD_SOURCE,
			parts: godParts,
		});
		expect(converted.titleId).toBe(original.titleId);
		expect(converted.contentType).toBe(original.contentType);
		expect(converted.version).toStrictEqual(original.version);
	});

	it('titleId is 8 uppercase hex digits', () => {
		const info = inspectSource(nullReadFn, iso.length, {
			source: GOD_SOURCE,
			parts: godParts,
		});
		expect(info.titleId).toMatch(/^[0-9A-F]{8}$/);
	});

	it('accepts a custom fetchSize without changing the result', () => {
		const info = inspectSource(nullReadFn, iso.length, {
			source: { ...GOD_SOURCE },
			parts: godParts,
		});
		expect(info.titleId).toBe('6A6A0003');
	});

	// The GOD writer's direct-passthrough mode (mode: 'none'/'partial')
	// streams raw source bytes instead of reauthoring a fresh XDVDFS image
	// the way full-rebuild mode ('full', the default above) does. Confirms
	// inspection round-trips through both modes' output, not just the
	// default one.
	it('parses a god fixture converted with mode: "none" (Direct backend)', () => {
		const noneIso = makeFixture({ titleId: 0x11110001 });
		const { dataParts: parts } = convertXisoFixtureToGodParts(noneIso, {
			...GOD_SOURCE,
			mode: 'none',
		});
		const info = inspectSource(nullReadFn, noneIso.length, {
			source: GOD_SOURCE,
			parts,
		});
		expect(info.titleId).toBe('11110001');
	});

	it('parses a god fixture converted with mode: "partial" (Direct backend, trim + zero)', () => {
		const partialIso = makeFixture({ titleId: 0x11110002 });
		const { dataParts: parts } = convertXisoFixtureToGodParts(partialIso, {
			...GOD_SOURCE,
			mode: 'partial',
		});
		const info = inspectSource(nullReadFn, partialIso.length, {
			source: GOD_SOURCE,
			parts,
		});
		expect(info.titleId).toBe('11110002');
	});

	it('detects Games on Demand content type for an x360-platform fixture converted to god', () => {
		const x360Iso = makeFixture({ titleId: 0x5a5a0003, platform: 'x360' });
		const { dataParts: parts } = convertXisoFixtureToGodParts(x360Iso);
		const info = inspectSource(nullReadFn, x360Iso.length, {
			source: GOD_SOURCE,
			parts,
		});
		expect(info.titleId).toBe('5A5A0003');
		expect(info.contentType).toBe('gamesOnDemand');
	});

	it('still throws for an invalid (zeroed) source declared as god', () => {
		const zeroedParts = godParts.map((part) => ({
			...part,
			readFn: (_offset: number, length: number) => new Uint8Array(length),
		}));
		expect(() =>
			inspectSource(nullReadFn, iso.length, {
				source: GOD_SOURCE,
				parts: zeroedParts,
			}),
		).toThrow();
	});

	it('still propagates errors thrown inside a part readFn', () => {
		const throwingParts = godParts.map((part) => ({
			...part,
			readFn: throwingReadFn,
		}));
		expect(() =>
			inspectSource(nullReadFn, iso.length, {
				source: GOD_SOURCE,
				parts: throwingParts,
			}),
		).toThrow('read error from JS');
	});

	it('detects Data#### parts regardless of filename casing (DATA, DaTa, etc.)', () => {
		const localIso = makeFixture({ titleId: 0x0badc0de });
		const { dataParts: realParts } = convertXisoFixtureToGodParts(localIso);
		expect(realParts).toHaveLength(1);
		const original = realParts[0];
		const casingVariants = [
			'DATA0000',
			'DaTa0000',
			'data0000',
			'dATA0000',
			'DATa0000',
		];
		for (const variantName of casingVariants) {
			const renamedPart = {
				...original,
				name: original.name.replace(/Data0000$/, variantName),
			};
			const info = inspectSource(nullReadFn, localIso.length, {
				source: GOD_SOURCE,
				parts: [renamedPart],
			});
			expect(info.titleId).toBe('0BADC0DE');
		}
	});

	it('throws if the sourceParts array is empty', () => {
		expect(() =>
			inspectSource(nullReadFn, iso.length, { source: GOD_SOURCE, parts: [] }),
		).toThrow();
	});

	// A real second Data%04d part can't be manufactured by slicing this
	// fixture's single converted part in half: total sector count is
	// derived from the last part's size, assuming every non-final part is
	// a *full* part-sized block, so an arbitrary half-sized "part 0" would
	// corrupt that derivation rather than exercise it. Genuine multi-part
	// GOD coverage needs a source large enough to actually cross the
	// part-size boundary.
	// Standalone GOD drops skip scanBatch entirely, bypassing the JS-side
	// partition sorting - they rely on the Rust code to sort parts
	// internally before reading the XDVDFS root offset.
	it('correctly sorts shuffled Data#### parts internally before reading, preventing root offset errors', () => {
		const localIso = makeFixture({ titleId: 0x47440150, platform: 'x360' });
		const { dataParts: realParts } = convertXisoFixtureToGodParts(localIso);
		// This fixture is small, so it produces exactly one part (Data0000).
		expect(realParts).toHaveLength(1);
		const realData0000 = realParts[0];
		const prefix = realData0000.name.replace(/Data0000$/, '');
		// Dummy parts to simulate a multi-part GOD candidate enumerating out
		// of order. The Rust parser requires parts to be at least one block
		// (4096 bytes), so these mirror the real part's size to pass length
		// checks.
		const dummyPart1 = {
			name: `${prefix}Data0001`,
			size: realData0000.size,
			readFn: (_offset: number, length: number) => new Uint8Array(length),
		};
		const dummyPart2 = {
			name: `${prefix}Data0002`,
			size: realData0000.size,
			readFn: (_offset: number, length: number) => new Uint8Array(length),
		};

		// Shuffled so Data0000 sits in the middle - if Rust fails to sort
		// them, it tries to read the XDVDFS root from dummyPart2 and throws
		// an immediate format error.
		const shuffledParts = [dummyPart2, realData0000, dummyPart1];
		expect(() => {
			const info = inspectSource(nullReadFn, localIso.length, {
				source: GOD_SOURCE,
				parts: shuffledParts,
			});
			expect(info.titleId).toBe('47440150');
		}).not.toThrow();
	});
});

describe('inspectSource with a `god` source: header-part content-type override', () => {
	it('reports the header-declared content type (installedGame) instead of inferring gamesOnDemand', () => {
		const { kv } = makeSyntheticKeyvault();
		const signedIso = makeFixture({ titleId: 0x6a6a0010, platform: 'x360' });
		const { dataParts, headerPart } = convertXisoFixtureToGodParts(signedIso, {
			format: 'god',
			signingKey: kv,
		});
		const info = inspectSource(nullReadFn, signedIso.length, {
			source: GOD_SOURCE,
			parts: [...dataParts, headerPart],
		});
		expect(info.titleId).toBe('6A6A0010');
		expect(info.contentType).toBe('installedGame');
	});

	it('rejects an unrecognized non-header, non-Data part with a clear error', () => {
		const { dataParts } = convertXisoFixtureToGodParts(
			makeFixture({ titleId: 0x1 }),
		);
		const junkPart = {
			name: 'thumbnail.png',
			size: 4096,
			readFn: () => new Uint8Array(4096),
		};
		expect(() =>
			inspectSource(nullReadFn, 0, {
				source: GOD_SOURCE,
				parts: [...dataParts, junkPart],
			}),
		).toThrow(/unrecognized part|not a LIVE\/PIRS\/CON-style header/);
	});

	it('sorts the header part out regardless of its position in sourceParts', () => {
		const { kv } = makeSyntheticKeyvault();
		const signedIso = makeFixture({ titleId: 0x6a6a0011, platform: 'x360' });
		const { dataParts, headerPart } = convertXisoFixtureToGodParts(signedIso, {
			format: 'god',
			signingKey: kv,
		});
		// Header first, then the data parts - the split in core::source::open
		// keys off the filename, not array position.
		const info = inspectSource(nullReadFn, signedIso.length, {
			source: GOD_SOURCE,
			parts: [headerPart, ...dataParts],
		});
		expect(info.contentType).toBe('installedGame');
	});

	it('extracts the same thumbnail whether or not the header override is present', () => {
		// Real assertion, not just "doesn't throw": is_xex has to come from
		// which executable TitleInfo::from_image actually found, not from
		// the (now-overridden) content_type - if that ordering regresses,
		// this looks for default.xbe on an XEX source and silently comes
		// back with a different (empty) thumbnail instead of matching.
		const { kv } = makeSyntheticKeyvault();
		const signedIso = makeFixture({
			titleId: 0x6a6a0012,
			platform: 'x360',
			thumbnail: {},
		});

		const { dataParts: unsignedParts } = convertXisoFixtureToGodParts(signedIso);
		const baseline = inspectSource(
			nullReadFn,
			signedIso.length,
			{ source: GOD_SOURCE, parts: unsignedParts },
			true,
		);
		expect(baseline.thumbnail).toBeDefined();

		const { dataParts, headerPart } = convertXisoFixtureToGodParts(signedIso, {
			format: 'god',
			signingKey: kv,
		});
		const overridden = inspectSource(
			nullReadFn,
			signedIso.length,
			{ source: GOD_SOURCE, parts: [...dataParts, headerPart] },
			true,
		);
		expect(overridden.contentType).toBe('installedGame');
		expect(overridden.thumbnail).toEqual(baseline.thumbnail);
	});

	it('throws when more than one non-Data part is supplied', () => {
		const { kv } = makeSyntheticKeyvault();
		const signedIso = makeFixture({ titleId: 0x6a6a0013, platform: 'x360' });
		const { dataParts, headerPart } = convertXisoFixtureToGodParts(signedIso, {
			format: 'god',
			signingKey: kv,
		});
		const bogusExtraPart = { ...headerPart, name: `${headerPart.name}.dup` };
		expect(() =>
			inspectSource(nullReadFn, signedIso.length, {
				source: GOD_SOURCE,
				parts: [...dataParts, headerPart, bogusExtraPart],
			}),
		).toThrow(/unexpected extra non-Data part/);
	});

	it('omitting the header falls back to the inferred gamesOnDemand content type', () => {
		const signedIso = makeFixture({ titleId: 0x6a6a0014, platform: 'x360' });
		const { dataParts } = convertXisoFixtureToGodParts(signedIso); // unsigned, no header
		const info = inspectSource(nullReadFn, signedIso.length, {
			source: GOD_SOURCE,
			parts: dataParts,
		});
		expect(info.contentType).toBe('gamesOnDemand');
	});
});

// titleThumbnail has no executable-based source at all (unlike thumbnail,
// which `thumbnail_from_image` can find directly off the launch
// executable) - for a `god` source it's read purely from an optional
// header part's Title Thumbnail Image field (0x571A). These tests build
// that header part by hand via `makeSyntheticGodHeaderPart` rather than
// driving a real signed write session, so the header's image bytes are
// under direct, exact control.
describe('inspectSource with a `god` source: header-part Title Thumbnail Image', () => {
	it('reports a header-declared titleThumbnail, independent of thumbnail', () => {
		const iso = makeFixture({ titleId: 0x6a6a0020, platform: 'x360' });
		const { dataParts } = convertXisoFixtureToGodParts(iso);
		const thumb = makeHeaderThumbnailBytes('THUMB');
		const titleThumb = makeHeaderThumbnailBytes('TITLE-THUMB');
		const headerPart = makeSyntheticGodHeaderPart({
			thumbnail: thumb,
			titleThumbnail: titleThumb,
		});
		const info = inspectSource(
			nullReadFn,
			iso.length,
			{ source: GOD_SOURCE, parts: [...dataParts, headerPart] },
			true,
		);
		expect(info.titleThumbnail).toEqual(titleThumb);
		// This fixture has no launch-executable icon of its own, so
		// `thumbnail` falls back to the same header the title thumbnail
		// came from - and the two fields are still independently readable.
		expect(info.thumbnail).toEqual(thumb);
		expect(info.thumbnail).not.toEqual(info.titleThumbnail);
	});

	it('leaves titleThumbnail undefined without a header part', () => {
		const iso = makeFixture({ titleId: 0x6a6a0021, platform: 'x360' });
		const { dataParts } = convertXisoFixtureToGodParts(iso);
		const info = inspectSource(
			nullReadFn,
			iso.length,
			{ source: GOD_SOURCE, parts: dataParts },
			true,
		);
		expect(info.titleThumbnail).toBeUndefined();
	});

	it('leaves titleThumbnail undefined when the header part carries no Title Thumbnail Image field, even with a content-type override', () => {
		const iso = makeFixture({ titleId: 0x6a6a0022, platform: 'x360' });
		const { dataParts } = convertXisoFixtureToGodParts(iso);
		const headerPart = makeSyntheticGodHeaderPart(); // content type only
		const info = inspectSource(
			nullReadFn,
			iso.length,
			{ source: GOD_SOURCE, parts: [...dataParts, headerPart] },
			true,
		);
		expect(info.contentType).toBe('installedGame');
		expect(info.titleThumbnail).toBeUndefined();
	});

	it('leaves titleThumbnail undefined when includeThumbnail is not requested, even if the header field is present', () => {
		const iso = makeFixture({ titleId: 0x6a6a0023, platform: 'x360' });
		const { dataParts } = convertXisoFixtureToGodParts(iso);
		const headerPart = makeSyntheticGodHeaderPart({
			titleThumbnail: makeHeaderThumbnailBytes('TITLE-THUMB'),
		});
		const info = inspectSource(nullReadFn, iso.length, {
			source: GOD_SOURCE,
			parts: [...dataParts, headerPart],
		});
		expect(info.titleThumbnail).toBeUndefined();
	});

	it('ignores a header Title Thumbnail Image field with a bad PNG magic, degrading to undefined rather than throwing', () => {
		const iso = makeFixture({ titleId: 0x6a6a0024, platform: 'x360' });
		const { dataParts } = convertXisoFixtureToGodParts(iso);
		const notPng = new TextEncoder().encode('not-a-png');
		const headerPart = makeSyntheticGodHeaderPart({ titleThumbnail: notPng });
		let info: ReturnType<typeof inspectSource> | undefined;
		expect(() => {
			info = inspectSource(
				nullReadFn,
				iso.length,
				{ source: GOD_SOURCE, parts: [...dataParts, headerPart] },
				true,
			);
		}).not.toThrow();
		expect(info?.titleThumbnail).toBeUndefined();
	});
});
