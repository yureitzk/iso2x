import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from './utils/wasm-setup.js';
import { sourceParts } from '../dist/detect-advanced.js';
import { splitSourceRef } from '../dist/index.js';
import type { SourceRef } from '../dist/types.js';
import { nameMap } from './utils/read-fns.js';

beforeAll(setupWasm);

// sourceParts() is a small, pure, publicly-exported building block used by
// resolveArbitraryXisoSplit, scanBatch, and resolveBatchEntry - but it was
// only ever exercised as an implementation detail of those calls, never
// tested directly.
describe('sourceParts()', () => {
	const files = nameMap({
		'disc1.iso': new Uint8Array([1, 2, 3, 4]),
		'disc2.iso': new Uint8Array([5, 6, 7]),
	});

	it('returns one SourcePart per name, in the same order as the input names array', () => {
		const parts = sourceParts(['disc1.iso', 'disc2.iso'], files);
		expect(parts.map((p) => p.name)).toEqual(['disc1.iso', 'disc2.iso']);
	});

	it("reports each part's size from the FileAccessor", () => {
		const parts = sourceParts(['disc1.iso', 'disc2.iso'], files);
		expect(parts.map((p) => p.size)).toEqual([4, 3]);
	});

	it("each part's readFn actually reads that file's bytes, not some other file's", () => {
		const parts = sourceParts(['disc1.iso', 'disc2.iso'], files);
		expect(parts[0]!.readFn(0, 4)).toEqual(new Uint8Array([1, 2, 3, 4]));
		expect(parts[1]!.readFn(0, 3)).toEqual(new Uint8Array([5, 6, 7]));
	});

	it('reverses order when the input names array is reversed (order is driven by the caller, not by file identity)', () => {
		const parts = sourceParts(['disc2.iso', 'disc1.iso'], files);
		expect(parts.map((p) => p.name)).toEqual(['disc2.iso', 'disc1.iso']);
		expect(parts[0]!.readFn(0, 3)).toEqual(new Uint8Array([5, 6, 7]));
	});

	it('returns an empty array for an empty names array', () => {
		expect(sourceParts([], files)).toEqual([]);
	});
});

// splitSourceRef() is the null-safe SourceRef destructure every one of
// ConversionSession.open/openSource/generateAttachXbe funnels through - but
// its `ref === undefined` branch specifically was never hit directly by any
// test, since every real caller in the suite always passes a defined
// SourceRef.
describe('splitSourceRef()', () => {
	it('returns { source: undefined, parts: undefined } when ref itself is undefined', () => {
		expect(splitSourceRef(undefined)).toEqual({
			source: undefined,
			parts: undefined,
		});
	});

	it('returns parts: undefined when ref.parts is omitted (single-file source)', () => {
		const ref: SourceRef = { source: { format: 'xiso' } };
		expect(splitSourceRef(ref)).toEqual({
			source: { format: 'xiso' },
			parts: undefined,
		});
	});

	it('passes both source and parts through unchanged when both are present', () => {
		const files = nameMap({ 'a.iso': new Uint8Array([9, 9]) });
		const parts = sourceParts(['a.iso'], files);
		const ref: SourceRef = { source: { format: 'xiso' }, parts };
		const result = splitSourceRef(ref);
		expect(result.source).toEqual({ format: 'xiso' });
		expect(result.parts).toBe(parts);
	});
});
