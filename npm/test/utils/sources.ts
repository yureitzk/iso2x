import type { SourceOptions, SourceRef } from '../../dist/types.js';

/**
 * Canonical per-format `SourceOptions`/`SourceRef` fixtures, shared
 * across tests so a source's declared format always matches the
 * `format` it actually carries.
 */

export const XISO_SOURCE_OPTIONS = {
	format: 'xiso',
} as const satisfies SourceOptions;
export const STFS_SOURCE_OPTIONS = {
	format: 'stfs',
} as const satisfies SourceOptions;
export const CCI_SOURCE_OPTIONS = {
	format: 'cci',
} as const satisfies SourceOptions;
export const CISO_SOURCE_OPTIONS = {
	format: 'ciso',
} as const satisfies SourceOptions;
export const ZAR_SOURCE_OPTIONS = {
	format: 'zar',
} as const satisfies SourceOptions;
export const GOD_SOURCE_OPTIONS = {
	format: 'god',
} as const satisfies SourceOptions;
export const EXTRACTED_SOURCE_OPTIONS = {
	format: 'extracted',
} as const satisfies SourceOptions;

export const XISO_SOURCE: SourceRef = { source: XISO_SOURCE_OPTIONS };
export const STFS_SOURCE: SourceRef = { source: STFS_SOURCE_OPTIONS };
export const CCI_SOURCE: SourceRef = { source: CCI_SOURCE_OPTIONS };
export const CISO_SOURCE: SourceRef = { source: CISO_SOURCE_OPTIONS };
export const ZAR_SOURCE: SourceRef = { source: ZAR_SOURCE_OPTIONS };
export const GOD_SOURCE: SourceRef = { source: GOD_SOURCE_OPTIONS };
export const EXTRACTED_SOURCE: SourceRef = {
	source: EXTRACTED_SOURCE_OPTIONS,
};
