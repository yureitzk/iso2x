import {
	inspectSource as rawInspectSource,
	openSource as rawOpenSource,
} from './wasm/iso2x.js';
import type { OpenedSource } from './wasm/iso2x.js';
import { splitSourceRef } from './types.js';
import type {
	SourceInfoResult,
	SourceOptions,
	SourcePart,
	SourceReadFn,
	SourceRef,
} from './types.js';

export type { OpenedSource } from './wasm/iso2x.js';

/**
 * Reads `source` once, walking the container/XDVDFS root just far
 * enough to answer `titleId`/`contentType`/etc. Pass
 * `includeThumbnail: true` to also decode the boot executable's
 * embedded thumbnail, if it has one.
 *
 * For a caller that wants to inspect and then convert or generate an
 * attach XBE from the same bytes without re-opening, use `openSource()`
 * below instead and call `OpenedSource.inspect()` on the handle.
 */
export function inspectSource(
	readFn: SourceReadFn,
	fileSize: number,
	ref: SourceRef,
	includeThumbnail?: boolean,
): SourceInfoResult {
	const { source, parts } = splitSourceRef(ref);
	return rawInspectSource(
		readFn,
		fileSize,
		source,
		parts,
		includeThumbnail ?? false,
	);
}

/**
 * Opens `source` once and returns a live handle. Pass the result to
 * `OpenedSource.inspect()`, `OpenedSource.generateAttachXbe()`, and/or
 * `OpenedSource.openConversionSession()`. `free()` the handle when done
 * with it, or let it get GC'd - see the class doc comment.
 */
export function openSource(
	readFn: SourceReadFn,
	fileSize: number,
	source: SourceOptions,
	parts?: SourcePart[],
	sequentialWindow?: number,
): OpenedSource {
	return rawOpenSource(readFn, fileSize, source, parts, sequentialWindow);
}
