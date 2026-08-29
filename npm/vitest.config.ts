import { defineConfig } from 'vitest/config';
import { playwright } from '@vitest/browser-playwright';

export default defineConfig({
	test: {
		projects: [
			{
				extends: true,
				test: {
					name: 'node',
					environment: 'node',
					include: ['test/**/*.test.ts'],
					exclude: ['test/browser/**'],
					// threads is required so wasm-global-setup.ts's
					// compiled WebAssembly.Module can be structured-cloned
					// to workers via provide()/inject() - the default
					// forks pool can't transfer it.
					pool: 'threads',
					globalSetup: ['./test/utils/wasm-global-setup.ts'],
				},
			},
			{
				extends: true,
				test: {
					name: 'browser',
					include: ['test/browser/**/*.browser.test.ts'],
					browser: {
						enabled: true,
						headless: true,
						screenshotFailures: false,
						provider: playwright({
							launchOptions: {
								args: ['--enable-features=SharedArrayBuffer'],
							},
						}),
						instances: [{ browser: 'chromium' }],
					},
				},
			},
		],
	},
});
