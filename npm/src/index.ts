import initWasm, {
	chainMhtDigest,
	lookupTitleById,
	suggestDiscTitle,
	formatTitleVersion,
	contentTypeFamily,
} from './wasm/iso2x.js';

export default initWasm;
export {
	chainMhtDigest,
	lookupTitleById,
	suggestDiscTitle,
	formatTitleVersion,
	contentTypeFamily,
};

export * from './types.js';
export * from './constants.js';

export * from './detect.js';
export * from './labels.js';

export * from './attach.js';
export * from './session.js';
export * from './source.js';
