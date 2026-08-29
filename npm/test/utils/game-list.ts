import { readFile } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));

export interface GameListEntry {
	id: number;
	name: string;
}

let cached: Promise<GameListEntry[]> | undefined;

/**
 * Parses crate/src/game_list/titles.jsonl directly - the same file
 * mod.rs's GAMES_BY_TITLE_ID table is generated from - so tests can
 * derive a real id/name pair from the current data instead of a
 * hand-typed literal that can drift out of sync.
 */
export function loadGameListEntries(): Promise<GameListEntry[]> {
	if (!cached) {
		cached = readGameListEntries();
	}
	return cached;
}

async function readGameListEntries(): Promise<GameListEntry[]> {
	const path = resolve(__dirname, '../../../crate/src/game_list/titles.jsonl');
	const text = await readFile(path, 'utf8');
	return text
		.split('\n')
		.filter((line) => line.trim().length > 0)
		.map((line): GameListEntry => {
			const row = JSON.parse(line) as { TitleID: string; Name: string };
			return { id: Number.parseInt(row.TitleID, 16), name: row.Name };
		})
		.sort((a, b) => a.id - b.id);
}

/** The lowest- and highest-id entries in the table, by id. */
export async function firstAndLastGameListEntries(): Promise<{
	first: GameListEntry;
	last: GameListEntry;
}> {
	const entries = await loadGameListEntries();
	return { first: entries[0], last: entries[entries.length - 1] };
}

/**
 * Finds the first place in the sorted table where two entries' ids
 * aren't adjacent, and returns those two entries plus an id that falls
 * in the gap between them.
 */
export async function findGameListGap(): Promise<{
	before: GameListEntry;
	after: GameListEntry;
	unmappedId: number;
}> {
	const entries = await loadGameListEntries();
	for (let i = 0; i < entries.length - 1; i++) {
		if (entries[i + 1].id - entries[i].id > 1) {
			return {
				before: entries[i],
				after: entries[i + 1],
				unmappedId: entries[i].id + 1,
			};
		}
	}
	throw new Error(
		'findGameListGap(): titles.jsonl is fully contiguous - no gap to find.',
	);
}
