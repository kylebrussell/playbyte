# Playbyte

Playbyte is an open-source, native app that presents a TikTok-like feed of
game save states ("Bytes"). Each feed item is a savestate that instantly loads
into an emulator, so you can jump straight into the action.

## Status

Playbyte is an early prototype and is under active development.

## Features

- TikTok-like feed UI for browsing and launching Bytes (savestates).
- Local, offline-first Byte storage.
- Supported systems: **NES**, **SNES**, **GBC**, **GBA** (via libretro cores).
- ROM library scanning by SHA-1 (Bytes reference ROM hashes; ROMs are never shipped).
- Official title detection via No-Intro `.dat` files (from `libretro-database`) with fuzzy matching.
- Cover art downloading from the libretro thumbnails repository, cached locally.
- Rename support for Bytes and ROM title fallbacks.
- Keyboard + controller navigation (DualSense touchpad swipe supported).

## Development

### Prerequisites

- **Rust** (stable) via [`rustup`](https://rustup.rs)
- **Git submodules** (libretro cores are vendored as submodules)
- **`make` + a C/C++ toolchain** (required to build the libretro cores)

### Clone

```sh
git clone --recursive <repo-url>
cd playbyte
```

If you already cloned without submodules:

```sh
git submodule update --init --recursive
```

### Build libretro cores

```sh
cargo xtask build-cores
```

This writes built cores to `dist/cores/`.

### Provide ROMs

Create `roms/` and add your ROM files (extensions currently recognized: `.nes`, `.sfc`, `.smc`, `.gb`, `.gbc`, `.gba`).

### Run the app

```sh
cargo run -p playbyte_app -- --roms ./roms --cores ./dist/cores --data ./data
```

## Directories & data files

The app uses three roots (overridable via flags):

- `--data`: Byte storage + caches (default `./data` when running from the repo)
- `--roms`: ROM search roots (default `./roms`)
- `--cores`: libretro cores (default `./cores`)

You’ll typically see the following appear under `data/`:

- `data/bytes/`: saved Bytes (metadata + thumbnail + compressed state)
- `data/romdb/`: cached No-Intro databases
- `data/covers/`: downloaded box art
- `data/rom_titles.json`: user-defined title overrides
- `data/rom_official_overrides.json`: manual “official title” selection overrides

## Controls (current)

- **PageUp / PageDown**: previous / next item
- **Tab**: toggle overlay
- **B**: create Byte (save state)
- **Controller L2 / R2**: previous / next item

## Docs

- ROM policy: [`docs/rom_policy.md`](docs/rom_policy.md)
- Core licensing matrix: [`docs/core_licenses.md`](docs/core_licenses.md)
- Optional backend (planned): [`docs/backend.md`](docs/backend.md)

## License

Playbyte is licensed under GPLv3. See [`LICENSE`](LICENSE).

Bundled libretro cores are separate projects under their own licenses; see
[`docs/core_licenses.md`](docs/core_licenses.md) and
[`vendor/libretro-cores/README.md`](vendor/libretro-cores/README.md).
