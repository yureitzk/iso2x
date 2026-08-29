/**
 * Minimal standalone decoder for a packed `.zar` archive's file tree.
 *
 * No wasm-exported API reads a zar's file listing back (`inspectSource`'s
 * `SourceInfo` carries title/version metadata only), so this decodes the
 * archive by hand as the inverse of the Rust writer's format. Used to
 * verify the writer never silently drops an entry.
 *
 * Format spec: `<https://github.com/Exzap/ZArchive#features--specifications>`
 */

// 6 SectionInfo (16 bytes each) + 32-byte hash + 8-byte total size + two
// 4-byte fields.
const FOOTER_SIZE = 6 * 16 + 32 + 8 + 4 + 4;

export interface ZarFileListEntry {
	path: string;
	size: number;
}

interface SectionInfo {
	offset: number;
	size: number;
}

function readSectionInfo(view: DataView, off: number): SectionInfo {
	// offset/size are u64 BE; Number() is safe here since test fixtures
	// stay far below Number.MAX_SAFE_INTEGER.
	return {
		offset: Number(view.getBigUint64(off, false)),
		size: Number(view.getBigUint64(off + 8, false)),
	};
}

/**
 * Decodes a packed ZAR archive's file tree directly: footer -> file-tree
 * section -> name table. Returns every file entry as a full forward-slash
 * path with its declared size, in the tree's own (BFS, case-insensitively
 * sorted per directory) order.
 */
export function readZarFileList(bytes: Uint8Array): ZarFileListEntry[] {
	const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
	const footerStart = bytes.length - FOOTER_SIZE;
	// 6 SectionInfo entries in fixed order: compressed_data, offset_records,
	// names, file_tree, meta_directory, meta_data.
	const names = readSectionInfo(view, footerStart + 2 * 16);
	const fileTree = readSectionInfo(view, footerStart + 3 * 16);

	// --- Name table: length-prefixed entries (1-byte header if len < 0x80,
	// 2-byte otherwise), decoded in the same order they were interned. ---
	const nameOffsetToString = new Map<number, string>();
	for (let cursor = 0; cursor < names.size;) {
		const entryStart = cursor;
		const b0 = bytes[names.offset + cursor];
		const twoByteHeader = (b0 & 0x80) !== 0;
		const headerLen = twoByteHeader ? 2 : 1;
		const len = twoByteHeader
			? (b0 & 0x7f) | (bytes[names.offset + cursor + 1] << 7)
			: b0;
		const nameBytes = bytes.subarray(
			names.offset + cursor + headerLen,
			names.offset + cursor + headerLen + len,
		);
		nameOffsetToString.set(entryStart, Buffer.from(nameBytes).toString('utf8'));
		cursor += headerLen + len;
	}

	// --- File tree: 16 bytes/entry, BFS order (same order the writer built it in). ---
	interface Node {
		isFile: boolean;
		name: string;
		fileSize: number;
		nodeStart: number;
		count: number;
	}
	const entryCount = fileTree.size / 16;
	const nodes: Node[] = [];
	for (let i = 0; i < entryCount; i++) {
		const base = fileTree.offset + i * 16;
		const flag = view.getUint32(base, false);
		const isFile = (flag & 0x8000_0000) !== 0;
		const nameOffsetRaw = flag & 0x7fff_ffff;
		const name =
			i === 0 ? '' : (nameOffsetToString.get(nameOffsetRaw) ?? '<unresolved>');
		if (isFile) {
			const sizeLow = view.getUint32(base + 8, false);
			const high = view.getUint32(base + 12, false);
			const fileSize = sizeLow + ((high >>> 16) & 0xffff) * 2 ** 32;
			nodes.push({ isFile: true, name, fileSize, nodeStart: 0, count: 0 });
		} else {
			const nodeStart = view.getUint32(base + 4, false);
			const count = view.getUint32(base + 8, false);
			nodes.push({ isFile: false, name, fileSize: 0, nodeStart, count });
		}
	}

	const files: ZarFileListEntry[] = [];
	function walk(idx: number, prefix: string) {
		const node = nodes[idx];
		if (node.isFile) {
			files.push({ path: prefix, size: node.fileSize });
			return;
		}
		for (let c = node.nodeStart; c < node.nodeStart + node.count; c++) {
			const childPath = prefix ? `${prefix}/${nodes[c].name}` : nodes[c].name;
			walk(c, childPath);
		}
	}
	walk(0, '');
	return files;
}
