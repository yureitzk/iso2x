import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from './utils/wasm-setup.js';
import { makeZarFixture } from './utils/fixtures/zar.js';
import { makeStfsFixture } from './utils/fixtures/stfs.js';
import { ConversionSession } from '../dist/index.js';
import { makeReadFn, nullReadFn } from './utils/read-fns.js';
import { EXTRACTED_SOURCE, STFS_SOURCE, ZAR_SOURCE } from './utils/sources.js';

beforeAll(setupWasm);

describe('zar source: read_name rejects an unsafe name-table entry', () => {
	it('throws at open() for a file named "../evil.txt"', () => {
		const evil = makeZarFixture('../evil.txt');
		expect(() =>
			ConversionSession.open(
				makeReadFn(evil),
				evil.length,
				{ format: 'extracted' },
				ZAR_SOURCE,
			),
		).toThrow(/unsafe path component/);
	});

	it('throws for an embedded separator ("a/b") smuggled into one name-table entry', () => {
		const evil = makeZarFixture('a/b');
		expect(() =>
			ConversionSession.open(
				makeReadFn(evil),
				evil.length,
				{ format: 'extracted' },
				ZAR_SOURCE,
			),
		).toThrow(/unsafe path component/);
	});

	it('opens a normal archive (an ordinary file name) without throwing, with it in the manifest', () => {
		const ok = makeZarFixture('normal-file.txt');
		const session = ConversionSession.open(
			makeReadFn(ok),
			ok.length,
			{ format: 'extracted' },
			ZAR_SOURCE,
		);
		expect(session.outputManifest()).toEqual([
			{ name: 'normal-file.txt', size: 0 },
		]);
		session.free();
	});
});

describe('stfs source: build_paths rejects an unsafe file-listing entry', () => {
	it('throws at open() for a file named "../evil.xex"', () => {
		const { bytes } = makeStfsFixture({ fileName: '../evil.xex' });
		expect(() =>
			ConversionSession.open(
				makeReadFn(bytes),
				bytes.length,
				{ format: 'extracted' },
				STFS_SOURCE,
			),
		).toThrow(/unsafe path component/);
	});

	it('throws for a bare ".." file-listing entry', () => {
		const { bytes } = makeStfsFixture({ fileName: '..' });
		expect(() =>
			ConversionSession.open(
				makeReadFn(bytes),
				bytes.length,
				{ format: 'extracted' },
				STFS_SOURCE,
			),
		).toThrow(/unsafe path component/);
	});

	it('does not reject an ordinary file name', () => {
		const { bytes } = makeStfsFixture({ fileName: 'default.xex' });
		const session = ConversionSession.open(
			makeReadFn(bytes),
			bytes.length,
			{ format: 'extracted' },
			STFS_SOURCE,
		);
		expect(session.outputManifest()).toEqual([
			{ name: 'default.xex', size: 0x100 },
		]);
		session.free();
	});
});

describe('extracted source: validate_names rejects an unsafe part path', () => {
	const bytes = new Uint8Array([1, 2, 3]);

	it.each([
		['leading traversal', '../evil.bin'],
		['traversal in a middle segment', 'dir/../../evil.bin'],
		['backslash traversal', '..\\evil.bin'],
		['bare dot segment', 'dir/./evil.bin'],
	])('throws for %s (%j)', (_label, name) => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'xiso' },
				{
					...EXTRACTED_SOURCE,
					parts: [{ name, size: bytes.length, readFn: makeReadFn(bytes) }],
				},
			),
		).toThrow(/unsafe path component/);
	});

	it('does not reject an ordinary nested path', () => {
		expect(() =>
			ConversionSession.open(
				nullReadFn,
				0,
				{ format: 'xiso' },
				{
					...EXTRACTED_SOURCE,
					parts: [
						{
							name: 'dir/default.xbe',
							size: bytes.length,
							readFn: makeReadFn(bytes),
						},
					],
				},
			),
		).not.toThrow(/unsafe path component/);
	});
});
