import type {
	SourceOptions,
	SourcePart,
	SourceReadFn,
	InvalidKind,
} from './wasm/iso2x.js';

export type { SourcePart, SourceReadFn };

export { ScrubMode, XisoMode } from './wasm/iso2x.js';

export type { SourceInfo as SourceInfoResult } from './wasm/iso2x.js';

export type { OutputManifestEntry } from './wasm/iso2x.js';

/**
 * `undefined` is a hard error, not a silent default - always resolve
 * via `detectFormat`/`detectDirFormat` first.
 */
export type { SourceOptions } from './wasm/iso2x.js';

export type ConversionFormat = SourceOptions['format'];

/**
 * - `'gamesOnDemand'` - Xbox 360 disc/GoD content, boots a `default.xex`.
 * - `'xboxOriginal'` - original Xbox (OGX) content, boots a `default.xbe`.
 * - `'arcadeGame'` - Xbox Live Arcade content, also boots a `default.xex`.
 *   Indistinguishable from `'gamesOnDemand'` by executable alone, so
 *   producing it from a non-STFS source requires the explicit
 *   `contentType` override on `OpenConversionSessionOptions`.
 */
export type { ContentType } from './wasm/iso2x.js';

/**
 * Which broad family a `ContentType` belongs to - see `contentTypeFamily`.
 * - `'bootable'` - carries a launch executable (`default.xex`/`default.xbe`).
 * - `'profileAccount'` - GPD-bearing profile data (currently just `'profile'`).
 * - `'titleAttached'` - names a *parent* title via `titleId` without being
 *   bootable itself (saves, DLC, avatar items, installers).
 * - `'standaloneAsset'` - everything else (themes, gamer pictures, movies,
 *   and other content with no meaningful parent title).
 */
export type { ContentFamily } from './wasm/iso2x.js';

export type { FormatOptions as OpenConversionSessionOptions } from './wasm/iso2x.js';

export type { InvalidKind } from './wasm/iso2x.js';

export type ResolvedSource =
	| {
			kind: 'file';
			format: ConversionFormat;
			readFn: SourceReadFn;
			fileSize: number;
	  }
	| {
			kind: 'dir';
			format: ConversionFormat;
			parts: SourcePart[];
	  }
	| {
			/**
			 * Named-but-invalid split pair - right filenames, wrong content
			 * - or an unnamed raw-XISO fragment set that's ambiguous or
			 * fails to verify. `invalidKind` tells a caller whether this is
			 * worth offering manual recovery for; don't infer it from
			 * `reason`, which is prose, not a stable contract.
			 */
			kind: 'invalid';
			names: string[];
			reason: string;
			invalidKind: InvalidKind;
	  };

export type { IsoRootOffsetCandidate } from './wasm/iso2x.js';

export type { CheckedEntry } from './wasm/iso2x.js';

/** `reason` is `undefined`, not `null`, when there's nothing to report. */
export type { SplitVerifyResult } from './wasm/iso2x.js';

export type { IsoCompletenessInfo } from './wasm/iso2x.js';

export type { RawXisoSplit } from './wasm/iso2x.js';

export type { DiscInfo } from './wasm/iso2x.js';

export type { BatchResolution } from './wasm/iso2x.js';

/**
 * Which format a source is, plus its parts if it's split across more
 * than one file. Paired with a separate `readFn`/`fileSize` argument by
 * `inspectSource`, `generateAttachXbe`, `ConversionSession.open`, and
 * `openSource`.
 */
export interface SourceRef {
	source: SourceOptions;
	parts?: SourcePart[];
}

/** Null-safe destructure of a `SourceRef` into `source`/`parts`. */
export function splitSourceRef(ref: SourceRef | undefined): {
	source: SourceOptions | undefined;
	parts: SourcePart[] | undefined;
} {
	return {
		source: ref?.source,
		parts: ref?.parts,
	};
}

/**
 * How to get at a named file's contents and size - the pairing every
 * batch-detection entry point in this package needs (`sourceParts`,
 * `scanBatch`, `resolveBatchEntry`, `resolveArbitraryXisoSplit`).
 */
export interface FileAccessor {
	readFn: (name: string) => SourceReadFn;
	size: (name: string) => number;
}
