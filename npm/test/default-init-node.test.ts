import { describe, it, expect } from 'vitest';
import initWasm from '../dist/index.js';

// No test elsewhere in this suite calls initWasm() with no arguments -
// every other file explicitly passes pre-read bytes (see wasm-setup.ts).
// That's the one init path a real consumer following typical wasm-bindgen
// usage (`import init from 'iso2x'; await init();`) would actually
// hit, and it resolves import.meta.url to a file:// URL under Node, which
// Node's fetch doesn't support. This pins down that known, current
// limitation as an explicit contract instead of a silent gap: Node
// consumers must pass explicit bytes (as this whole suite already does),
// and this test fails loudly if that ever silently starts working or
// starts failing a different way.
describe('default init() under plain Node (no pre-supplied bytes)', () => {
	it('rejects because Node fetch cannot resolve a file:// URL', async () => {
		await expect(initWasm()).rejects.toThrow(/fetch/i);
	});
});
