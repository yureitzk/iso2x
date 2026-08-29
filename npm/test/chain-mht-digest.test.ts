import { describe, it, expect, beforeAll } from 'vitest';
import { chainMhtDigest, mhtSize } from '../dist/index.js';
import { setupWasm } from './utils/wasm-setup.js';

beforeAll(setupWasm);

describe('chainMhtDigest', () => {
	const validMht = () => new Uint8Array(mhtSize());
	const validDigest = () => new Uint8Array(20).fill(0xab);

	it('rejects a digest shorter than 20 bytes', () => {
		expect(() => chainMhtDigest(validMht(), new Uint8Array(10))).toThrow();
	});

	it('rejects a digest longer than 20 bytes', () => {
		expect(() => chainMhtDigest(validMht(), new Uint8Array(21))).toThrow();
	});

	it('rejects an empty digest', () => {
		expect(() => chainMhtDigest(validMht(), new Uint8Array(0))).toThrow();
	});

	it('returns a 20-byte result for valid inputs', () => {
		const result = chainMhtDigest(validMht(), validDigest());
		expect(result).toBeInstanceOf(Uint8Array);
		expect(result.length).toBe(20);
	});

	it('is deterministic', () => {
		const r1 = chainMhtDigest(validMht(), validDigest());
		const r2 = chainMhtDigest(validMht(), validDigest());
		expect(r1).toEqual(r2);
	});

	it('produces different results for different digests', () => {
		const r1 = chainMhtDigest(validMht(), new Uint8Array(20).fill(0x01));
		const r2 = chainMhtDigest(validMht(), new Uint8Array(20).fill(0x02));
		expect(r1).not.toEqual(r2);
	});

	it('accumulates state - chaining changes the result', () => {
		const digest = validDigest();
		const firstResult = chainMhtDigest(validMht(), digest);
		const chainedMht = validMht();
		chainedMht.set(firstResult, 0);
		const r2 = chainMhtDigest(chainedMht, digest);
		expect(firstResult).not.toEqual(r2);
	});
});
