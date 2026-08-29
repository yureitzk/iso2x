import {
	resolveBatch as rawResolveBatch,
	resolveBatchEntry as rawResolveBatchEntry,
} from './wasm/iso2x.js';
import { sourceParts } from './detect-advanced.js';
import type { BatchResolution, FileAccessor, ResolvedSource } from './types.js';

/**
 * Single dropped file: cheap magic-byte detection (xiso/ciso/cci).
 * Dropped folder: shape-based detection (god/extracted). `entries` is
 * every regular file's path, relative to the folder root. Returns
 * `undefined` if the folder is neither god- nor extracted-shaped -
 * treat that as a batch dir and resolve it with `scanBatch`/
 * `resolveBatchEntry` below.
 */
export { detectDirFormat, detectFormat } from './wasm/iso2x.js';

/**
 * Classifies every loose file in a batch dir at once: independently
 * complete images (grouped into a `MultiDiscSet` when several share a
 * title/disc-count), content-verified raw XISO splits, and anything
 * left unresolved. Non-XISO-magic entries are reported back as
 * `Unresolved` rather than silently dropped.
 *
 * For per-file diagnostic detail instead of a finished classification,
 * see `detect-advanced.ts`.
 */
export async function scanBatch(
	entries: string[],
	files: FileAccessor,
	onItem?: (result: BatchResolution) => void,
): Promise<BatchResolution[]> {
	const parts = sourceParts(entries, files);
	return rawResolveBatch(parts, onItem);
}

/**
 * Resolves one entry of a batch dir: standalone file, split image, or a
 * named-but-invalid split pair. `entries[0]` is the file being
 * resolved; the rest are candidate siblings for split detection. Use
 * this when files arrive one at a time; use `scanBatch` instead when
 * the whole batch is available up front.
 */
export async function resolveBatchEntry(
	entries: string[],
	files: FileAccessor,
): Promise<ResolvedSource> {
	const parts = sourceParts(entries, files);
	const resolved = rawResolveBatchEntry(parts);

	switch (resolved.kind) {
		case 'file':
			return {
				kind: 'file',
				format: resolved.format,
				readFn: files.readFn(resolved.name),
				fileSize: files.size(resolved.name),
			};
		case 'dir':
			return {
				kind: 'dir',
				format: resolved.format,
				parts: sourceParts(resolved.names, files),
			};
		case 'invalid':
			return {
				kind: 'invalid',
				names: resolved.names,
				reason: resolved.reason,
				invalidKind: resolved.invalidKind,
			};
	}
}
