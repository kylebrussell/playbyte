This directory holds vendored libretro core source trees as git submodules.

## Cores

| Core | System | Repository | Version |
|------|--------|------------|---------|
| Mesen | NES | [libretro/Mesen](https://github.com/libretro/Mesen) | 0.9.9 |
| bsnes | SNES | [libretro/bsnes-libretro](https://github.com/libretro/bsnes-libretro) | v115 |
| Gambatte | GBC | [libretro/gambatte-libretro](https://github.com/libretro/gambatte-libretro) | 9fe223d |
| mGBA | GBA | [libretro/mgba](https://github.com/libretro/mgba) | c758314 |

## Setup

After cloning the repository, initialize the submodules:

```bash
git submodule update --init --recursive
```

## Building

Build and bundle cores with:

```bash
cargo xtask build-cores
```

Built cores are placed in `dist/cores/`.
