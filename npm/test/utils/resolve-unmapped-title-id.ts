import { lookupTitleById } from '../../dist/index.js';

// Gap between id 0x2 and 0x111 in the real table means this resolves by
// the third call; the ceiling just exists to fail loudly instead of
// looping forever if lookupTitleById() is broken.
const PROBE_ATTEMPTS = 10_000;

/**
 * Returns a title id that lookupTitleById() confirms, right now, has no
 * entry in the live table - instead of a hardcoded literal assumed to
 * be free. titles.jsonl is vendored, externally-sourced data that gets
 * regenerated periodically, so a fixed "safe" id can silently start
 * colliding with a real game later.
 *
 * Must be called after initWasm() has resolved.
 */
export function resolveUnmappedTitleId(): number {
	// Start at 1, not 0 - id 0 is a genuine hit ("Retroarch").
	for (let candidate = 1; candidate <= PROBE_ATTEMPTS; candidate++) {
		if (lookupTitleById(candidate) === undefined) {
			return candidate;
		}
	}
	throw new Error(
		`resolveUnmappedTitleId(): every id from 1 to ${PROBE_ATTEMPTS} resolved to a ` +
			'real title - either titles.jsonl has grown implausibly dense at the low end, ' +
			'or lookupTitleById() is broken and matching everything.',
	);
}
