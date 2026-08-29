import { generateAttachXbe as rawGenerateAttachXbe } from './wasm/iso2x.js';
import { splitSourceRef } from './types.js';
import type { SourceRef, SourceReadFn } from './types.js';

export function generateAttachXbe(
	readFn: SourceReadFn,
	fileSize: number,
	ref: SourceRef,
): Uint8Array {
	const { source, parts } = splitSourceRef(ref);
	return rawGenerateAttachXbe(readFn, fileSize, source, parts);
}
