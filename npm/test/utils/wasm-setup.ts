import initWasm from '../../dist/index.js';
import { readFile } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { inject } from 'vitest';

const __dirname = dirname(fileURLToPath(import.meta.url));

let initialized = false;

export async function setupWasm() {
	if (initialized) return;
	// Reuses the wasm Module precompiled once in wasm-global-setup.ts.
	// Falls back to compiling locally when there's no vitest context (e.g. a
	// standalone script) - inject() throws rather than returning undefined in
	// that case, hence the try/catch.
	let precompiled: WebAssembly.Module | undefined;
	try {
		precompiled = inject('wasmModule');
	} catch {
		precompiled = undefined;
	}
	if (precompiled) {
		await initWasm({ module_or_path: precompiled });
	} else {
		const wasmPath = resolve(__dirname, '../../dist/wasm/iso2x_bg.wasm');
		const wasmBuffer = await readFile(wasmPath);
		await initWasm({ module_or_path: wasmBuffer });
	}
	initialized = true;
}
