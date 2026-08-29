import { readFile } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { TestProject } from 'vitest/node';

const __dirname = dirname(fileURLToPath(import.meta.url));

// Compiling is the expensive, stateless part of wasm-bindgen init and is
// safe to share; instantiating (done per test file in wasm-setup.ts) is
// cheap and gives each file its own linear memory, same as today. This
// requires pool: 'threads' - the default 'forks' pool can't structured-
// clone a WebAssembly.Module across the process boundary and crashes.
export default async function setup(project: TestProject) {
	const wasmPath = resolve(__dirname, '../../dist/wasm/iso2x_bg.wasm');
	const wasmBuffer = await readFile(wasmPath);
	const wasmModule = await WebAssembly.compile(wasmBuffer);
	project.provide('wasmModule', wasmModule);

	// Separate instantiation used only to resolve an unmapped title id
	// once here, instead of per test file. See resolve-unmapped-title-id.ts.
	const { default: initWasm } = await import('../../dist/index.js');
	await initWasm({ module_or_path: wasmModule });
	const { resolveUnmappedTitleId } =
		await import('./resolve-unmapped-title-id.js');
	project.provide('unmappedTitleId', resolveUnmappedTitleId());
}

declare module 'vitest' {
	export interface ProvidedContext {
		wasmModule: WebAssembly.Module;
		unmappedTitleId: number;
	}
}
