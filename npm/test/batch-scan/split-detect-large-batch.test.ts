import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { checkIsoCompleteness } from '../../dist/detect-advanced.js';
import { makeReadFn, scan, namesOf } from '../utils/read-fns.js';

beforeAll(async () => {
	await setupWasm();
});

// crypto.getRandomValues caps at 65,536 bytes per call.
function fillRandom(bytes: Uint8Array): void {
	const maxChunk = 65536;
	for (let offset = 0; offset < bytes.length; offset += maxChunk) {
		crypto.getRandomValues(bytes.subarray(offset, offset + maxChunk));
	}
}

describe('find_raw_split large-batch regression', () => {
	it('does not hang or crash on a large batch when the header has no matching continuation anywhere in it', async () => {
		// Only the header goes into the batch; its continuation is withheld.
		const splitSource = makeFixture({ titleId: 0x4d495810 });
		const info = checkIsoCompleteness(
			makeReadFn(splitSource),
			splitSource.length,
		);
		expect(info).toBeDefined();
		expect(info!.isComplete).toBe(true);
		const cut = info!.rootOffset + info!.maxUsedPrefixSize - 0x400;
		const header = splitSource.slice(0, cut);

		const files: Record<string, Uint8Array> = {
			'orphan-header.iso': header,
		};

		// A large field of same-shaped-but-unrelated bystanders.
		const bystanderSize = Math.max(header.length, 0x8000);
		for (let i = 0; i < 300; i++) {
			const bytes = new Uint8Array(bystanderSize);
			fillRandom(bytes);
			files[`bystander-${i.toString().padStart(3, '0')}.iso`] = bytes;
		}

		const start = Date.now();
		const results = await scan(files);
		const elapsedMs = Date.now() - start;

		expect(elapsedMs).toBeLessThan(15000);

		// Every file must still be accounted for exactly once.
		const allNames = results.flatMap(namesOf).sort();
		expect(allNames).toEqual(Object.keys(files).sort());

		const rawSplits = results.filter((r) => r.kind === 'rawSplit');
		expect(
			rawSplits,
			`expected no rawSplit result since the real continuation was withheld, got ${JSON.stringify(rawSplits)}`,
		).toHaveLength(0);

		const orphanResult = results.find((r) =>
			namesOf(r).includes('orphan-header.iso'),
		);
		expect(orphanResult).toBeDefined();
		expect(orphanResult!.kind).toBe('unresolved');
	});
});
