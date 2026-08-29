import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { openSource } from '../../dist/index.js';
import type { OpenedSource } from '../../dist/index.js';
import { makeReadFn } from '../utils/read-fns.js';

beforeAll(async () => {
	await setupWasm();
});

function openHandle(titleId: number): OpenedSource {
	const iso = makeFixture({ titleId });
	return openSource(makeReadFn(iso), iso.length, { format: 'xiso' });
}

// wasm-bindgen zeroes the wrapped object's pointer on the *first*
// free()/[Symbol.dispose](), then rejects any further use of that
// pointer - so double-free, and use-after-free generally, become a
// safe, catchable JS exception rather than memory corruption. See
// https://github.com/wasm-bindgen/wasm-bindgen - the "null pointer
// passed to rust" message below is wasm-bindgen's own guard, not
// anything this project's code throws itself.
describe('OpenedSource double-free / use-after-free', () => {
	it('a second free() throws instead of silently succeeding', () => {
		const handle = openHandle(0x52000001);
		handle.free();
		expect(() => handle.free()).toThrow('null pointer passed to rust');
	});

	it('a third free() throws the same way as the second', () => {
		const handle = openHandle(0x52000002);
		handle.free();
		expect(() => handle.free()).toThrow('null pointer passed to rust');
		expect(() => handle.free()).toThrow('null pointer passed to rust');
	});

	it('calling a method after free() throws instead of touching freed memory', () => {
		const handle = openHandle(0x52000003);
		handle.free();
		expect(() => handle.inspect(false)).toThrow('null pointer passed to rust');
		expect(() => handle.generateAttachXbe()).toThrow(
			'null pointer passed to rust',
		);
	});

	it('[Symbol.dispose]() then free() throws - dispose already consumed the handle', () => {
		const handle = openHandle(0x52000004);
		handle[Symbol.dispose]();
		expect(() => handle.free()).toThrow('null pointer passed to rust');
	});

	it('free() then [Symbol.dispose]() throws - the reverse order behaves the same way', () => {
		const handle = openHandle(0x52000005);
		handle.free();
		expect(() => handle[Symbol.dispose]()).toThrow('null pointer passed to rust');
	});
});
