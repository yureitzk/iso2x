/**
 * Hand-built, minimal `.zar` archive with exactly one file entry named
 * `name` - a general-purpose fixture, not specific to adversarial input.
 * `name` can be an ordinary filename (a normal, structurally valid
 * archive) or an adversarial one (e.g. `../evil.txt`), since both are
 * just bytes to this builder; `path-traversal.test.ts` uses it for both.
 *
 * Every file is 0 bytes, so no compressed data block is ever produced -
 * this only builds the footer/name-table/file-tree, not a full archive.
 *
 * Layout: `[name table][file tree (32 bytes: root dir + one file entry)][144-byte footer]`.
 *
 * Spec: <https://github.com/Exzap/ZArchive#features--specifications>
 */

const FOOTER_SIZE = 6 * 16 + 32 + 8 + 4 + 4; // 144
const FOOTER_MAGIC = 0x169f_52d6;
const FOOTER_VERSION = 0x61bf_3a01;

function writeSectionInfo(
	view: DataView,
	offset: number,
	sectionOffset: number,
	size: number,
): void {
	view.setBigUint64(offset, BigInt(sectionOffset), false);
	view.setBigUint64(offset + 8, BigInt(size), false);
}

/** Builds a minimal, structurally-valid `.zar` archive with one file, named exactly `name`. */
export function makeZarFixture(name: string): Uint8Array {
	if (name.length >= 0x80) {
		throw new Error(
			'makeZarFixture only implements the 1-byte (short-form) name-table header',
		);
	}

	// --- name table: one length-prefixed entry ---
	const nameBytes = Uint8Array.from(name, (c) => c.charCodeAt(0));
	const nameTable = new Uint8Array(1 + nameBytes.length);
	nameTable[0] = nameBytes.length;
	nameTable.set(nameBytes, 1);

	// --- file tree: entry 0 = root dir (1 child at index 1), entry 1 = the file ---
	const fileTree = new Uint8Array(32);
	const treeView = new DataView(fileTree.buffer);
	// root dir: flag=0 (dir, name_offset=0/unused), node_start=1, count=1
	treeView.setUint32(0, 0, false);
	treeView.setUint32(4, 1, false);
	treeView.setUint32(8, 1, false);
	treeView.setUint32(12, 0, false);
	// file entry: flag=0x8000_0000 (is_file, name_offset=0), offset=0, size=0
	treeView.setUint32(16, 0x8000_0000, false);
	treeView.setUint32(20, 0, false);
	treeView.setUint32(24, 0, false);
	treeView.setUint32(28, 0, false);

	const namesOffset = 0;
	const fileTreeOffset = nameTable.length;
	const footerOffset = fileTreeOffset + fileTree.length;
	const totalSize = footerOffset + FOOTER_SIZE;

	const bytes = new Uint8Array(totalSize);
	bytes.set(nameTable, namesOffset);
	bytes.set(fileTree, fileTreeOffset);

	const view = new DataView(bytes.buffer);
	let o = footerOffset;
	writeSectionInfo(view, o, 0, 0); // compressed_data
	o += 16;
	writeSectionInfo(view, o, 0, 0); // offset_records
	o += 16;
	writeSectionInfo(view, o, namesOffset, nameTable.length); // names
	o += 16;
	writeSectionInfo(view, o, fileTreeOffset, fileTree.length); // file_tree
	o += 16;
	writeSectionInfo(view, o, footerOffset, 0); // meta_directory
	o += 16;
	writeSectionInfo(view, o, footerOffset, 0); // meta_data
	o += 16;
	// hash: 32 zero bytes - `open()` never verifies it (documented limitation).
	o += 32;
	view.setBigUint64(o, BigInt(totalSize), false); // total_size
	o += 8;
	view.setUint32(o, FOOTER_VERSION, false); // version
	o += 4;
	view.setUint32(o, FOOTER_MAGIC, false); // magic

	return bytes;
}
