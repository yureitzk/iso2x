import { describe, it, expect, beforeAll } from 'vitest';
import { setupWasm } from '../utils/wasm-setup.js';
import { makeFixture } from '../utils/fixtures/xsf.js';
import { inspectSource } from '../../dist/index.js';
import { makeReadFn, nullReadFn } from '../utils/read-fns.js';
import {
	convertXisoFixtureToBytes,
	convertXisoFixtureToGodParts,
	convertXisoFixtureToExtractedParts,
} from '../utils/session-helpers.js';
import { makeStfsFixture } from '../utils/fixtures/stfs.js';

beforeAll(setupWasm);

describe('ciso/cci source declarations distinguish fixtures from each other', () => {
	it('returns different titleIds for different ciso-converted fixtures', () => {
		const isoA = makeFixture({ titleId: 0x41560001 });
		const isoB = makeFixture({ titleId: 0xdeadbeef });
		const cisoA = convertXisoFixtureToBytes(isoA, {
			format: 'ciso',
			outputName: 'a',
		});
		const cisoB = convertXisoFixtureToBytes(isoB, {
			format: 'ciso',
			outputName: 'b',
		});
		const infoA = inspectSource(makeReadFn(cisoA), cisoA.length, {
			source: { format: 'ciso' },
		});
		const infoB = inspectSource(makeReadFn(cisoB), cisoB.length, {
			source: { format: 'ciso' },
		});
		expect(infoA.titleId).not.toBe(infoB.titleId);
	});

	it('rejects ciso-converted bytes opened as a cci source', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const cisoBytes = convertXisoFixtureToBytes(iso, {
			format: 'ciso',
			outputName: 'game',
		});
		expect(() =>
			inspectSource(makeReadFn(cisoBytes), cisoBytes.length, {
				source: { format: 'cci' },
			}),
		).toThrow();
	});

	it('rejects cci-converted bytes opened as a ciso source', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const cciBytes = convertXisoFixtureToBytes(iso, {
			format: 'cci',
			outputName: 'game',
		});
		expect(() =>
			inspectSource(makeReadFn(cciBytes), cciBytes.length, {
				source: { format: 'ciso' },
			}),
		).toThrow();
	});
});

describe('god source declarations distinguish fixtures from each other and reject foreign bytes', () => {
	it('returns different titleIds for different god-converted fixtures', () => {
		const isoA = makeFixture({ titleId: 0x41560001 });
		const isoB = makeFixture({ titleId: 0xdeadbeef });
		const { dataParts: partsA } = convertXisoFixtureToGodParts(isoA);
		const { dataParts: partsB } = convertXisoFixtureToGodParts(isoB);
		const infoA = inspectSource(nullReadFn, isoA.length, {
			source: { format: 'god' },
			parts: partsA,
		});
		const infoB = inspectSource(nullReadFn, isoB.length, {
			source: { format: 'god' },
			parts: partsB,
		});
		expect(infoA.titleId).not.toBe(infoB.titleId);
	});

	it('rejects a ciso-converted file opened as a god source', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const cisoBytes = convertXisoFixtureToBytes(iso, {
			format: 'ciso',
			outputName: 'game',
		});
		expect(() =>
			inspectSource(nullReadFn, cisoBytes.length, {
				source: { format: 'god' },
				parts: [
					{
						name: 'game.data/Data0000',
						size: cisoBytes.length,
						readFn: makeReadFn(cisoBytes),
					},
				],
			}),
		).toThrow();
	});

	it('rejects a god Data part opened as an xiso source', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const { dataParts: parts } = convertXisoFixtureToGodParts(iso);
		expect(() =>
			inspectSource(nullReadFn, parts[0].size, {
				source: { format: 'xiso' },
				parts: [
					{ name: parts[0].name, size: parts[0].size, readFn: parts[0].readFn },
				],
			}),
		).toThrow();
	});

	it("rejects an extracted source's loose files opened as a god source", () => {
		// A god source expects each part to be a Data%04d MHT-hashed
		// container, not a raw extracted file - opening one should fail to
		// parse the first (and only) extracted part's bytes as one.
		const iso = makeFixture({ titleId: 0x41560001 });
		const extractedParts = convertXisoFixtureToExtractedParts(iso);
		expect(() =>
			inspectSource(nullReadFn, extractedParts[0].size, {
				source: { format: 'god' },
				parts: extractedParts,
			}),
		).toThrow();
	});
});

describe('stfs source declarations reject foreign bytes', () => {
	it('rejects an xiso-shaped file opened as an stfs source', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		expect(() =>
			inspectSource(makeReadFn(iso), iso.length, {
				source: { format: 'stfs' },
			}),
		).toThrow();
	});

	it('rejects an stfs-shaped file opened as an xiso source', () => {
		const { bytes: stfsBytes } = makeStfsFixture();
		expect(() =>
			inspectSource(makeReadFn(stfsBytes), stfsBytes.length, {
				source: { format: 'xiso' },
			}),
		).toThrow();
	});
});

describe('zar source declarations distinguish fixtures from each other and reject foreign bytes', () => {
	it('rejects a ciso-converted file opened as a zar source', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const cisoBytes = convertXisoFixtureToBytes(iso, {
			format: 'ciso',
			outputName: 'game',
		});
		expect(() =>
			inspectSource(makeReadFn(cisoBytes), cisoBytes.length, {
				source: { format: 'zar' },
			}),
		).toThrow();
	});

	it('rejects a zar-converted file opened as a ciso source', () => {
		const iso = makeFixture({ titleId: 0x41560001 });
		const zarBytes = convertXisoFixtureToBytes(iso, {
			format: 'zar',
			outputName: 'game',
		});
		expect(() =>
			inspectSource(makeReadFn(zarBytes), zarBytes.length, {
				source: { format: 'ciso' },
			}),
		).toThrow();
	});

	it('returns different titleIds for different zar-converted fixtures', () => {
		const isoA = makeFixture({ titleId: 0x41560001 });
		const isoB = makeFixture({ titleId: 0xdeadbeef });
		const zarA = convertXisoFixtureToBytes(isoA, {
			format: 'zar',
			outputName: 'a',
		});
		const zarB = convertXisoFixtureToBytes(isoB, {
			format: 'zar',
			outputName: 'b',
		});
		const infoA = inspectSource(makeReadFn(zarA), zarA.length, {
			source: { format: 'zar' },
		});
		const infoB = inspectSource(makeReadFn(zarB), zarB.length, {
			source: { format: 'zar' },
		});
		expect(infoA.titleId).not.toBe(infoB.titleId);
	});
});

describe('inspectSource with an invalid `source` option', () => {
	it('throws for an unrecognized format tag', () => {
		expect(() =>
			inspectSource(nullReadFn, 10 * 1024 * 1024, {
				source: {
					// @ts-expect-error - deliberately invalid `format` to exercise the
					// error path for an unrecognized source-format tag.
					format: 'invalid',
				},
			}),
		).toThrow();
	});
});
