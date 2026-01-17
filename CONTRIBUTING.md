# Contributing

Thanks for considering a contribution to Playbyte.

## Quick start

- Install the Rust toolchain from <https://rustup.rs>.
- Clone with submodules: `git clone --recursive <repo-url>`, or if already cloned run `git submodule update --init --recursive`.
- Build libretro cores: `cargo xtask build-cores`.
- Run `cargo fmt` and `cargo clippy --workspace --all-targets` before opening a PR.

## Licensing

By contributing to this repository, you agree that your contributions are
licensed under the GPLv3 license of the project.

## Issues & discussions

- Open an issue before large changes.
- Use clear, minimal repro steps for bugs.
