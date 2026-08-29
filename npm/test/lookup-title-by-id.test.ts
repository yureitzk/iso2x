import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from './utils/wasm-setup.js';
import { lookupTitleById } from '../dist/index.js';
import {
	loadGameListEntries,
	firstAndLastGameListEntries,
	findGameListGap,
} from './utils/game-list.js';

beforeAll(async () => {
	await setupWasm();
});

// Expected id/name pairs are derived from titles.jsonl (see
// utils/game-list.ts) rather than hardcoded, since it's vendored,
// externally-sourced data that gets regenerated periodically.
describe('lookupTitleById()', () => {
	it('finds the first table entry', async () => {
		const { first } = await firstAndLastGameListEntries();
		expect(lookupTitleById(first.id)).toBe(first.name);
	});

	it('finds an entry in the middle of the table', async () => {
		const entries = await loadGameListEntries();
		const middle = entries[Math.floor(entries.length / 2)];
		expect(lookupTitleById(middle.id)).toBe(middle.name);
	});

	it('finds the last table entry, at the top of the u32 range actually used', async () => {
		const { last } = await firstAndLastGameListEntries();
		expect(lookupTitleById(last.id)).toBe(last.name);
	});

	it('returns undefined for an id that falls in a gap between two real entries', async () => {
		// Confirms this doesn't fall back to a nearest-neighbor match.
		const { before, after, unmappedId } = await findGameListGap();
		expect(lookupTitleById(before.id)).toBe(before.name);
		expect(lookupTitleById(after.id)).toBe(after.name);
		expect(lookupTitleById(unmappedId)).toBeUndefined();
	});

	it('returns undefined for a title id above every entry in the table', () => {
		expect(lookupTitleById(0xffffffff)).toBeUndefined();
	});

	it('title id 0 is a genuine hit, not a "not found" default value', async () => {
		// Guards against an implementation that returns titleId 0's name
		// as a fallback for "not found" instead of `undefined`.
		const entries = await loadGameListEntries();
		const zero = entries.find((entry) => entry.id === 0);
		expect(zero, 'expected titles.jsonl to have an entry at id 0').toBeDefined();
		expect(lookupTitleById(0)).toBe(zero!.name);
		expect(lookupTitleById(0)).not.toBeUndefined();
	});
});
