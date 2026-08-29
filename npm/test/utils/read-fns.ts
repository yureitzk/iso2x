import { scanBatch } from '../../dist/index.js';
import type {
	BatchResolution,
	FileAccessor,
	SourceReadFn,
} from '../../dist/index.js';
import { expect } from 'vitest';

export function makeReadFn(iso: Uint8Array) {
	return (offset: number, length: number): Uint8Array =>
		iso.slice(offset, offset + length);
}

/** Like `makeReadFn`, but zero-fills reads past `source`'s actual length. */
export function makeSparseReadFn(source: Uint8Array) {
	return (offset: number, length: number): Uint8Array => {
		if (offset >= source.length) return new Uint8Array(length);
		const slice = source.slice(offset, offset + length);
		if (slice.length < length) {
			const out = new Uint8Array(length);
			out.set(slice);
			return out;
		}
		return slice;
	};
}

export const nullReadFn = (_offset: number, length: number) =>
	new Uint8Array(length);

export const throwingReadFn = (
	_offset: number,
	_length: number,
): Uint8Array => {
	throw new Error('read error from JS');
};

/**
 * Wraps a readFn and records every (offset, length) call made against it.
 */
export function makeSpyReadFn(inner: ReturnType<typeof makeSparseReadFn>) {
	const calls: { offset: number; length: number }[] = [];
	const spy = (offset: number, length: number) => {
		calls.push({ offset, length });
		return inner(offset, length);
	};
	return { spy, calls };
}

/** True if any recorded call's byte range covers `pos`. */
export function sawFetchCovering(
	calls: { offset: number; length: number }[],
	pos: number,
): boolean {
	return calls.some((c) => c.offset <= pos && c.offset + c.length > pos);
}

/** Builds a `FileAccessor` over an in-memory name -> bytes map. */
export function nameMap(files: Record<string, Uint8Array>): FileAccessor {
	return {
		readFn: (name: string): SourceReadFn => makeReadFn(files[name]!),
		size: (name: string) => files[name]!.length,
	};
}

export function scan(
	files: Record<string, Uint8Array>,
): Promise<BatchResolution[]> {
	return scanBatch(Object.keys(files), nameMap(files));
}

/** Asserts exactly one `kind` result is present in `results` and returns it. */
export function only<K extends BatchResolution['kind']>(
	results: BatchResolution[],
	kind: K,
): Extract<BatchResolution, { kind: K }> {
	const matches = results.filter((r) => r.kind === kind);
	expect(
		matches,
		`expected exactly one "${kind}" result in ${JSON.stringify(results)}`,
	).toHaveLength(1);
	return matches[0] as Extract<BatchResolution, { kind: K }>;
}

export function namesOf(r: BatchResolution): string[] {
	switch (r.kind) {
		case 'multiDiscSet':
			return r.discs.map((d) => d.name);
		case 'rawSplit':
			return r.parts;
		case 'godFolder':
		case 'standalone':
		case 'unresolved':
			return r.names;
	}
}
