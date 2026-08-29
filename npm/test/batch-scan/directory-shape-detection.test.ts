import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { detectDirFormat } from '../../dist/index.js';
import { convertXisoFixtureToExtractedParts } from '../utils/session-helpers.js';

// STFS packages are content-addressed by SHA1, so the same package can
// appear under multiple paths with an identical filename - not a collision.
const CONTENT_PATHS = [
	'Content/0000000000000000/504787D8/000D0000/281104A67F7C961E2736A5F6F524D43FC81EB79158',
	'Content/0000000000000000/584107EF/000D0000/4EBF4EDF58B9F1E642FD604B56844DBC2615A20A58',
	'Content/0000000000000000/58410889/000D0000/87D0A5D366F24C8FF8BEE120E5F72D78F100E18958',
	'Content/0000000000000000/584109FF/000D0000/281104A67F7C961E2736A5F6F524D43FC81EB79158',
] as const;

const NON_MAGIC_ASSET_PATHS = [
	'AvatarAssetPack/nxeart',
	'en/Music.xwb',
	'fr/Sounds.xwb',
	'jp/XBLAVolume3.xgs',
	'button.png',
] as const;

describe('directory-shape detection', () => {
	beforeAll(async () => {
		await setupWasm();
	});

	it('detects a GOD folder regardless of Data#### ordering in the listing', () => {
		const entries = [
			'Game.data/Data0002',
			'Game.data/Data0000',
			'Game.data/Data0001',
		];
		expect(detectDirFormat(entries)).toBe('god');
	});

	it('detects an extracted folder from a top-level launch executable alone', () => {
		const entries = ['default.xex', 'nxeart', 'en/Music.xwb'];
		expect(detectDirFormat(entries)).toBe('extracted');
	});

	it('returns undefined for a folder that matches neither shape (e.g. loose split-ISO parts)', () => {
		const entries = ['split.1.iso', 'split.2.iso'];
		expect(detectDirFormat(entries)).toBeUndefined();
	});

	it('detects a GOD folder genuinely split across multiple Data#### parts even when their case is non-standard', () => {
		const entries = [
			'Game.data/dATa0002',
			'Game.data/DATA0000',
			'Game.data/data0001',
		];
		expect(detectDirFormat(entries)).toBe('god');
	});

	it('still resolves as extracted when a real Content/ package tree sits underneath the game files', async () => {
		// Content/ is separate from the disc/XDVDFS filesystem and must not
		// change detectDirFormat's result.
		const xiso = makeFixture({
			titleId: 0x4d495808,
			platform: 'x360',
			includeSystemUpdate: true,
		});
		const extractedParts = convertXisoFixtureToExtractedParts(xiso);

		const entries = [
			...extractedParts.map((p) => p.name),
			...CONTENT_PATHS,
			...NON_MAGIC_ASSET_PATHS,
		];
		expect(detectDirFormat(entries)).toBe('extracted');
	});
});
