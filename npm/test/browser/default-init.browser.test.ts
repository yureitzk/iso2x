import { describe, it, expect } from 'vitest';
import initWasm, { lookupTitleById } from '../../dist/index.js';

// Runs in a real browser (see the 'browser' project in vitest.config.ts),
// not Node and not happy-dom. This is the one init path nothing else in
// the suite exercises: default-init.browser.test.ts calls initWasm() with
// no arguments, which resolves import.meta.url against the page's real
// origin and fetches it - the mirror of test/default-init-node.test.ts,
// which pins down that the same call fails under plain Node.
describe('default init() in a real browser', () => {
	it('resolves via fetch against the page origin and produces a working module', async () => {
		await initWasm();
		// A real call, not just "the promise resolved" - proves the
		// instantiated module actually works. 0xffffffff is a structural
		// boundary (top of the u32 range), not game-list content.
		expect(() => lookupTitleById(0xffffffff)).not.toThrow();
		expect(lookupTitleById(0xffffffff)).toBeUndefined();
	});
});
