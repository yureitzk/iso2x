import {
	ConversionSession as RawConversionSession,
	openConversionSession as rawOpenConversionSession,
} from './wasm/iso2x.js';
import { splitSourceRef } from './types.js';
import type {
	OpenConversionSessionOptions,
	OutputManifestEntry,
	SourceReadFn,
	SourceRef,
} from './types.js';

/**
 * One pull-based session type for every output format:
 *
 *   const session = ConversionSession.open(readFn, fileSize, options, ref);
 *   while (!session.isDone()) {
 *     const chunk = session.nextChunk(maxBytes);
 *     if (chunk) yield chunk;
 *   }
 *   session.free();
 *
 * For 'god', 'ciso', and 'cci', hashing/sizing must be driven to
 * completion before any nextChunk() calls - see hashNextPart(). For
 * 'extracted', 'ciso', and 'cci', call currentEntryName() after each
 * nextChunk() to know which output file the chunk belongs to; 'god' and
 * 'xiso' return null there, since they're a single anonymous stream.
 * 'zar' also always produces one stream, but currentEntryName() there
 * is non-null.
 */
export class ConversionSession {
	private constructor(private readonly raw: RawConversionSession) {}

	/**
	 * Wraps an already-open raw wasm-bindgen session - e.g. one
	 * returned by `OpenedSource.openConversionSession()` - in this
	 * same `null`-based API. `OpenedSource.openConversionSession()`
	 * can't return the wrapped type itself (it's generated
	 * wasm-bindgen glue), so callers going through it should wrap the
	 * result with this before using it.
	 */
	static wrap(raw: RawConversionSession): ConversionSession {
		return new ConversionSession(raw);
	}

	[Symbol.dispose](): void {
		this.raw[Symbol.dispose]();
	}

	static open(
		readFn: SourceReadFn,
		fileSize: number,
		options: OpenConversionSessionOptions,
		ref: SourceRef,
		/**
		 * Readahead window (bytes) for the bulk sequential pass. Only
		 * affects god/xiso/ciso/cci sources. Omit to use the reader's
		 * own tuned default (8 MiB).
		 */
		sequentialWindow?: number,
	): ConversionSession {
		const { source, parts } = splitSourceRef(ref);
		return new ConversionSession(
			rawOpenConversionSession(
				readFn,
				fileSize,
				options,
				source,
				parts,
				sequentialWindow,
			),
		);
	}

	/**
	 * For 'god', 'ciso', and 'cci': hashes/sizes one more part (or
	 * sizing batch) and returns whether that pre-streaming pass is now
	 * complete. nextChunk() throws if called before this returns true
	 * for those formats. For 'xiso', 'extracted', and 'zar' this is a
	 * no-op that returns true immediately.
	 *
	 * Call this in a loop with an await between calls so bounded
	 * per-call cost gives the caller's event loop control back
	 * regularly.
	 */
	hashNextPart(): boolean {
		return this.raw.hashNextPart();
	}

	/** Returns the next chunk, or null once the session is exhausted. */
	nextChunk(maxBytes: number): Uint8Array<ArrayBuffer> | null {
		return (
			(this.raw.nextChunk(maxBytes) as Uint8Array<ArrayBuffer> | undefined) ?? null
		);
	}

	isDone(): boolean {
		return this.raw.isDone();
	}

	/**
	 * Completed `totalUnits()` count, for formats where `nextChunk()`'s
	 * returned byte length isn't a reliable progress proxy (currently
	 * only 'zar' - its chunks are compressed output bytes, while
	 * `totalUnits()` is raw input bytes). `null` for every other format.
	 */
	unitsDone(): number | null {
		return this.raw.unitsDone() ?? null;
	}

	/** Sectors for xiso, parts for god, file count for extracted/ciso/cci, total input bytes for zar. */
	totalUnits(): number {
		return this.raw.totalUnits();
	}

	/**
	 * `{ name, size }` per output file for formats that produce more
	 * than one (god, extracted, ciso, cci). 'zar' also populates this,
	 * even though it only produces one output file, but stays empty
	 * until streaming reaches the footer phase, like ciso/cci.
	 *
	 * Safe to call any time after open() for 'god'/'extracted'. Empty
	 * until hashNextPart()/streaming completes for 'ciso'/'cci'/'zar'.
	 * Empty for 'xiso' - use `totalUnits() * 2048` instead.
	 */
	outputManifest(): OutputManifestEntry[] {
		return this.raw.outputManifest();
	}

	/**
	 * Name of the file the most recent nextChunk() call's bytes belong
	 * to. Meaningful for 'extracted', 'ciso', 'cci', and 'zar'; null
	 * for 'god' and 'xiso'.
	 */
	currentEntryName(): string | null {
		return this.raw.currentEntryName() ?? null;
	}

	free(): void {
		this.raw.free();
	}
}

/**
 * Drop-in async generator for a chunk-streaming loop. Yields bytes in
 * order for any format; doesn't route chunks to different output
 * files - callers using 'extracted'/'ciso'/'cci' must check
 * session.currentEntryName() after each chunk and switch output files
 * when it changes.
 *
 * For 'god', 'ciso', and 'cci' sessions, the caller is responsible for
 * driving session.hashNextPart() to completion before iterating this
 * generator.
 */
export async function* chunksFromSession(
	session: ConversionSession,
	maxBytes: number,
	waitForRoom: () => Promise<void>,
): AsyncGenerator<Uint8Array> {
	while (!session.isDone()) {
		await waitForRoom();
		const chunk = session.nextChunk(maxBytes);
		if (chunk === null) break;
		yield chunk;
	}
}
