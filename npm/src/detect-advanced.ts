/**
 * Low-level split-detection primitives used by `scanBatch`/
 * `resolveBatchEntry` in `detect.ts`. Hands back intermediate detection
 * state (is this file complete? did this ordering verify?) instead of a
 * finished classification - useful for a progressive/diagnostic UI. If
 * you just want to know what's in a folder, use `detect.ts` instead.
 */
import {
	checkIsoCompleteness as rawCheckIsoCompleteness,
	resolveArbitraryXisoSplit as rawResolveArbitraryXisoSplit,
	verifySplitCandidate as rawVerifySplitCandidate,
	type IsoCompletenessResult,
} from './wasm/iso2x.js';
import type {
	FileAccessor,
	RawXisoSplit,
	SourcePart,
	SourceReadFn,
	SplitVerifyResult,
} from './types.js';

/** Builds `SourceParts` for a set of filenames from a `FileAccessor`. */
export function sourceParts(
	names: string[],
	files: FileAccessor,
): SourcePart[] {
	return names.map((name) => ({
		name,
		size: files.size(name),
		readFn: files.readFn(name),
	}));
}

/**
 * Content-verified raw XISO split detection over any set of filenames -
 * no naming convention required. Returns `null` unless exactly one entry
 * is a truncated XDVDFS header with a headerless continuation fragment
 * to pair it with. Independently complete images (including a genuine
 * multi-disc set) are never treated as fragments of one another.
 */
export async function resolveArbitraryXisoSplit(
	entries: string[],
	files: FileAccessor,
): Promise<RawXisoSplit | null> {
	const parts = sourceParts(entries, files);
	return rawResolveArbitraryXisoSplit(parts) ?? null;
}

/**
 * Single-file completeness probe: does this file alone hold every byte
 * its own directory table references, or is it a truncated fragment?
 */
export function checkIsoCompleteness(
	readFn: SourceReadFn,
	fileSize: number,
): IsoCompletenessResult {
	return rawCheckIsoCompleteness(readFn, fileSize);
}

/** Verifies one candidate split ordering. */
export function verifySplitCandidate(parts: SourcePart[]): SplitVerifyResult {
	return rawVerifySplitCandidate(parts);
}
