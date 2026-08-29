# iso2x

An Xbox disc image converter library, which compiles to WebAssembly. Designed
around streaming: sources are read on demand via a callback, and output is
produced in chunks, so large images never need to sit fully in memory.

Supported environments:

- Node - read files in chunks, write output to disk or a stream.
- Browser - read a `File`/`Blob` via `slice()`, stream output to wherever you
  like.

## Demo

[iso2x-web](https://github.com/yureitzk/iso2x-web) - a browser-based tool built
using this library.

## Formats

Supported formats, as both source and target:

- [xiso](https://consolemods.org/wiki/Xbox:Playing_Game_Backups#%22XISO%22) -
  the raw XDVDFS disc image used by both the original Xbox and Xbox 360. Write
  mode (`XisoMode`): `'full'` (default, full XDVDFS reauthor), `'trim'` (cut
  trailing padding only), or `'zero'` (zero unused sectors in place, no trim -
  Xbox 360 images are trimmed instead in this mode). Splits past ~4.28 GB into
  `name.N.xiso.iso` parts.

- [god](https://en.wikipedia.org/wiki/Xbox_Games_Store#Games_on_Demand) - Xbox
  360 Games-on-Demand, an STFS-family container that splits a title into
  `Data%04d` parts, each hash-verified against a master/sub hash tree. Uses the
  same `ScrubMode` as `ciso`/`cci` below.

- [stfs](https://free60.org/System-Software/Formats/STFS/) - Xbox 360's
  general-purpose package format (`CON`/`LIVE`/`PIRS` header), the same family
  `god` is built on but used for non-disc content: profiles, saved games, DLC,
  arcade titles. Also the target for profile/save transfer.

- [cci](https://consolemods.org/wiki/Xbox:Repackinator) - Cerbios Compressed
  Image, developed by Team Resurgent with Team Cerbios; per-sector
  LZ4-compressed, each split part fully self-contained with its own header and
  index. Uses `ScrubMode`: `'none'` (straight copy), `'partial'` (trim + zero
  interior gaps), or `'full'` (default, reauthor). Splits past ~4.28 GB.

- [ciso](https://github.com/antangelo/ciso) - Compressed ISO, per-sector
  LZ4-compressed with a shared index table in part 1. Same `ScrubMode` options
  as `cci`. Splits past ~4 GiB (Stellar's CSO layout).

- [zar](https://github.com/Exzap/ZArchive) - a zstd-compressed, name-addressed
  archive. No `ScrubMode` equivalent.

- [extracted](https://free60.org/System-Software/Formats/XEX/) - plain files,
  either straight from an ISO's XDVDFS tree or unpacked from a `zar`/`stfs`
  archive; also how XEX executables are reached for patching/inspection.

Any of the above can be converted to any other (subject to source/target
compatibility). Extracted-folder sources can convert to any target format;
image-backed sources (xiso/ciso/cci/god/zar/stfs files) can too - the crate
opens whichever backing the resolved source needs per target.

## Building from source

Requirements: `cargo`, `wasm-bindgen-cli`, `wasm-opt` (from
[Binaryen](https://github.com/WebAssembly/binaryen)), `node`

```sh
./build.sh
```

This runs the Rust crate's test suite, compiles it to `wasm32-unknown-unknown`,
runs `wasm-bindgen` to generate JS/TS bindings, optimizes the binary with
`wasm-opt`, and builds the `npm/` TypeScript package (copying the generated wasm
bindings into `npm/dist/wasm`).

---

## TODO

- extend STFS package support
- add native (non-wasm) usage
- consider a non-streaming path for environments where that overhead isn't worth
  it

## Attribution

- [xdvdfs](https://github.com/antangelo/xdvdfs) by antangelo
- [iso2god-rs](https://github.com/iliazeus/iso2god-rs) by iliazeus
- [XGDTool](https://github.com/wiredopposite/XGDTool) by wiredopposite
- [extract-xiso](https://github.com/XboxDev/extract-xiso) by XboxDev
- [Repackinator](https://github.com/Team-Resurgent/Repackinator) by Team
  Resurgent
- [attach-xbe-builder](https://github.com/greguz/attach-xbe-builder) by greguz
- [XboxToolkit](https://github.com/Team-Resurgent/XboxToolkit) by Team Resurgent
- [ZArchive](https://github.com/Exzap/ZArchive) by Exzap
- [Velocity](https://github.com/hetelek/velocity) by hetelek
- [XboxUnity](https://xboxunity.net) by Phoenix

## License

[MIT](./LICENSE)
