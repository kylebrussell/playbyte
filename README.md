# Playbyte

**Skip to the good part.**

Playbyte lets you jump straight into the best moments of retro games. Browse a
carousel of savestates—called "Bytes"—and load any of them instantly. No menus,
no intros, no grinding to get back to where it gets fun.

## Why Playbyte?

- **Instant gratification** — Every Byte drops you directly into gameplay. Pick
  a boss fight, a tricky platforming section, or a favorite level and you're
  playing in under a second.
- **Your games, your moments** — Create Bytes from your own ROM collection.
  Capture the exact frame you want to revisit or share.
- **Works offline** — Everything runs locally. Your ROMs and savestates stay on
  your machine.
- **Controller-friendly** — Play with keyboard or gamepad. DualSense touchpad
  swipe is supported for browsing.

## Supported Systems

NES, SNES, Game Boy Color, and Game Boy Advance (via libretro cores).

## How It Works

1. Point Playbyte at a folder of ROMs you legally own.
2. The app scans and identifies your games automatically (using No-Intro
   databases) and fetches cover art.
3. Play a game, then press **B** to save a Byte at any moment.
4. Browse your Bytes in the carousel and jump back in whenever you want.

## Status

Playbyte is an early prototype under active development. Expect rough edges.

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

## Controls

### App Navigation

| Action | Keyboard | Controller |
|--------|----------|------------|
| Previous / Next Byte | PageUp / PageDown | L2 / R2 |
| Toggle overlay | Tab | — |
| Create Byte | B | — |

### In-Game (Keyboard)

| Game Input | Key |
|------------|-----|
| D-Pad | Arrow keys |
| A | X |
| B | Z |
| Start | Enter |
| Select | Shift |

## Docs

- ROM policy: [`docs/rom_policy.md`](docs/rom_policy.md)
- Core licensing matrix: [`docs/core_licenses.md`](docs/core_licenses.md)
- Optional backend (planned): [`docs/backend.md`](docs/backend.md)

## License

Playbyte is licensed under GPLv3. See [`LICENSE`](LICENSE).

Bundled libretro cores are separate projects under their own licenses; see
[`docs/core_licenses.md`](docs/core_licenses.md) and
[`vendor/libretro-cores/README.md`](vendor/libretro-cores/README.md).
