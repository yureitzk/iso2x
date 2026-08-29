/**
 * Byte helpers shared across fixture builders and the tests that read
 * their output back.
 */

/** Writes `s` as raw ASCII bytes (no NUL terminator) at `offset`. */
export function writeAscii(buf: Uint8Array, offset: number, s: string): void {
	for (let i = 0; i < s.length; i++) {
		buf[offset + i] = s.charCodeAt(i);
	}
}

export function readU32LE(buf: Uint8Array, offset: number): number {
	return (
		(buf[offset] |
			(buf[offset + 1] << 8) |
			(buf[offset + 2] << 16) |
			(buf[offset + 3] << 24)) >>>
		0
	);
}

/** XBE certificate address (relocated), from the base/cert address fields
 * at 0x104/0x118 - see xsf.ts's XBE stub layout doc comment. */
export function certAddr(buf: Uint8Array): number {
	const baseAddress = readU32LE(buf, 0x104);
	const certificateAddress = readU32LE(buf, 0x118);
	return certificateAddress - baseAddress;
}
