import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { ConversionSession } from '../../dist/index.js';
import type { SourceRef } from '../../dist/index.js';
import { makeReadFn } from '../utils/read-fns.js';

beforeAll(async () => {
	await setupWasm();
});

const XISO_SOURCE: SourceRef = { source: { format: 'xiso' } };

function openSession(titleId: number): ConversionSession {
	const iso = makeFixture({ titleId });
	return ConversionSession.open(
		makeReadFn(iso),
		iso.length,
		{ format: 'xiso' },
		XISO_SOURCE,
	);
}

// wasm-bindgen zeroes the wrapped object's pointer on the *first*
// free()/[Symbol.dispose](), then rejects any further use of that
// pointer - so double-free, and use-after-free generally, become a
// safe, catchable JS exception rather than memory corruption. See
// https://github.com/wasm-bindgen/wasm-bindgen - the "null pointer
// passed to rust" message below is wasm-bindgen's own guard, not
// anything this project's code throws itself.
describe('ConversionSession double-free / use-after-free', () => {
	it('a second free() throws instead of silently succeeding', () => {
		const session = openSession(0x51000001);
		session.free();
		expect(() => session.free()).toThrow('null pointer passed to rust');
	});

	it('a third free() throws the same way as the second', () => {
		const session = openSession(0x51000002);
		session.free();
		expect(() => session.free()).toThrow('null pointer passed to rust');
		expect(() => session.free()).toThrow('null pointer passed to rust');
	});

	it('calling a method after free() throws instead of touching freed memory', () => {
		const session = openSession(0x51000003);
		session.free();
		expect(() => session.isDone()).toThrow('null pointer passed to rust');
		expect(() => session.nextChunk(100)).toThrow('null pointer passed to rust');
	});

	it('[Symbol.dispose]() then free() throws - dispose already consumed the handle', () => {
		const session = openSession(0x51000004);
		session[Symbol.dispose]();
		expect(() => session.free()).toThrow('null pointer passed to rust');
	});

	it('free() then [Symbol.dispose]() throws - the reverse order behaves the same way', () => {
		const session = openSession(0x51000005);
		session.free();
		expect(() => session[Symbol.dispose]()).toThrow(
			'null pointer passed to rust',
		);
	});
});
