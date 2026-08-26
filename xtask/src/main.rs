use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct CoreSpec {
    id: &'static str,
    dir: &'static str,
    makefile_dir: &'static str,
    output: &'static str,
    output_dir: &'static str,
    /// Extra make arguments (e.g., target=libretro)
    make_args: &'static [&'static str],
}

const CORES: &[CoreSpec] = &[
    CoreSpec {
        id: "mesen",
        dir: "mesen",
        makefile_dir: "Libretro",
        output: "mesen_libretro",
        output_dir: "",
        // Clear ARCHFLAGS to avoid building universal binary (i386+x86_64)
        make_args: &["ARCHFLAGS="],
    },
    CoreSpec {
        id: "bsnes",
        dir: "bsnes",
        makefile_dir: "bsnes",
        output: "bsnes_libretro",
        output_dir: "out",
        make_args: &["target=libretro", "local=false"],
    },
    CoreSpec {
        id: "gambatte",
        dir: "gambatte",
        makefile_dir: "",
        output: "gambatte_libretro",
        output_dir: "",
        make_args: &[],
    },
    CoreSpec {
        id: "mgba",
        dir: "mgba",
        makefile_dir: "",
        output: "mgba_libretro",
        output_dir: "",
        // macOS has locale_t but Makefile.libretro doesn't define HAVE_LOCALE for osx
        make_args: &["PLATFORM_DEFINES=-DHAVE_LOCALE"],
    },
];

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("build-cores") => build_cores(),
        Some("package") => package(),
        _ => {
            eprintln!("Usage: cargo xtask <build-cores|package>");
            Ok(())
        }
    }
}

fn build_cores() -> Result<()> {
    let ext = core_extension();
    let dist_dir = PathBuf::from("dist/cores");
    fs::create_dir_all(&dist_dir)?;

    for core in CORES {
        let core_base = Path::new("vendor/libretro-cores").join(core.dir);
        if !core_base.exists() {
            bail!("Missing core source at {}", core_base.display());
        }

        // Build directory is core_base + makefile_dir
        let build_dir = if core.makefile_dir.is_empty() {
            core_base.clone()
        } else {
            core_base.join(core.makefile_dir)
        };

        let mut cmd = Command::new("make");
        cmd.current_dir(&build_dir);

        // Add any core-specific make arguments
        for arg in core.make_args {
            cmd.arg(*arg);
        }

        // Platform-specific workarounds for vendored core Makefiles.
        if cfg!(target_os = "windows") {
            match core.id {
                // mesen/gambatte/mgba detect the platform via `uname -s` and
                // match `MINGW` or lowercase `win`; MSYS-based Windows
                // runners report `MSYS_NT-...`, which matches neither, so
                // they fall through to the unix target and produce a `.so`.
                // Passing `platform=win` selects the mingw-compatible
                // gcc/g++ branch that produces a `.dll`.
                "mesen" | "gambatte" | "mgba" => {
                    cmd.arg("platform=win");
                }
                // bsnes's nall build system detects Windows via the OS env
                // var (so `platform=win` would actually break it - nall
                // expects `windows`). Instead: build in library mode so the
                // standalone-application link libraries are omitted. Also
                // stop nall's Windows guard from clobbering
                // __MSVCRT_VERSION__ (see ensure_bsnes_msvcrt_default).
                "bsnes" => {
                    cmd.arg("binary=library");
                    ensure_bsnes_msvcrt_default()?;
                }
                _ => {}
            }
        } else if cfg!(target_os = "linux") && core.id == "bsnes" {
            // Build in library mode: the GNUmakefile evaluates `binary :=
            // application` (its default) before target-libretro switches to
            // library mode, leaking `-lX11 -lXext` into the core's link line,
            // which fails on headless CI runners without X11 dev libraries.
            cmd.arg("binary=library");
        }

        if core.id == "bsnes" {
            ensure_bsnes_stdexcept()?;
        }

        println!("Building {} in {} ...", core.id, build_dir.display());
        let status = cmd
            .status()
            .with_context(|| format!("failed to build core {}", core.id))?;
        if !status.success() {
            bail!("core build failed for {}", core.id);
        }

        // Output path: build_dir + output_dir + output.ext
        let output_path = if core.output_dir.is_empty() {
            build_dir.join(format!("{}.{}", core.output, ext))
        } else {
            build_dir
                .join(core.output_dir)
                .join(format!("{}.{}", core.output, ext))
        };

        if !output_path.exists() {
            bail!("missing built core output at {}", output_path.display());
        }

        let target_path = dist_dir.join(output_path.file_name().unwrap());
        fs::copy(&output_path, &target_path)?;
        println!("Bundled {} -> {}", core.id, target_path.display());
    }

    Ok(())
}

fn package() -> Result<()> {
    let platform = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let package_dir = PathBuf::from("dist/package").join(platform);
    fs::create_dir_all(&package_dir)?;

    let binary_name = if cfg!(target_os = "windows") {
        "playbyte_app.exe"
    } else {
        "playbyte_app"
    };
    let binary_src = PathBuf::from("target/release").join(binary_name);
    if !binary_src.exists() {
        bail!(
            "missing binary at {} (run cargo build --release)",
            binary_src.display()
        );
    }

    let binary_dst = package_dir.join(binary_name);
    fs::copy(&binary_src, &binary_dst)?;

    let cores_src = PathBuf::from("dist/cores");
    if cores_src.exists() {
        let cores_dst = package_dir.join("cores");
        fs::create_dir_all(&cores_dst)?;
        for entry in fs::read_dir(cores_src)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let dst = cores_dst.join(entry.file_name());
                fs::copy(entry.path(), dst)?;
            }
        }
    }

    fs::copy("LICENSE", package_dir.join("LICENSE"))?;
    fs::copy("README.md", package_dir.join("README.md"))?;

    println!("Packaged app at {}", package_dir.display());
    Ok(())
}

/// Ensure bsnes's bundled nall headers can find std::runtime_error.
///
/// nall/arithmetic/natural.hpp uses std::runtime_error without including
/// <stdexcept>; newer GCC no longer provides it transitively. Patching the
/// header directly (idempotently) is more robust than a `compiler=` make
/// override, which would also apply to C sources where `g++ -x c` cannot
/// find C++ headers. Mutates the vendored submodule working tree only;
/// macOS is skipped because it builds cleanly unpatched.
fn ensure_bsnes_stdexcept() -> Result<()> {
    if cfg!(target_os = "macos") {
        return Ok(());
    }
    let path = Path::new("vendor/libretro-cores/bsnes/nall/arithmetic/natural.hpp");
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if content.contains("#include <stdexcept>") {
        return Ok(());
    }
    println!("Patching {} to include <stdexcept> ...", path.display());
    fs::write(path, format!("#include <stdexcept>\n{content}"))
        .with_context(|| format!("failed to patch {}", path.display()))?;
    Ok(())
}

/// Stop bsnes's nall Windows guard from clobbering __MSVCRT_VERSION__.
///
/// nall/windows/guard.hpp pins `__MSVCRT_VERSION__` to WINVER (0x0601) before
/// any Windows header is included. _mingw.h later derives `_UCRT` from
/// __MSVCRT_VERSION__ (>= 0x1400 or 0xE00) and mingw-w64's stdlib.h only
/// declares quick_exit/at_quick_exit when _UCRT is defined - so the guard's
/// low pin makes libstdc++'s <cstdlib> fail to compile on UCRT toolchains
/// (the GitHub runner's mingw64 GCC 15), no matter what _WIN32_WINNT is set
/// to. Removing the pin lets the toolchain default apply. Idempotent;
/// Windows only.
fn ensure_bsnes_msvcrt_default() -> Result<()> {
    if !cfg!(target_os = "windows") {
        return Ok(());
    }
    let path = Path::new("vendor/libretro-cores/bsnes/nall/windows/guard.hpp");
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let pin = "#define __MSVCRT_VERSION__ WINVER";
    if !content.contains(pin) {
        return Ok(());
    }
    let mut patched = content
        .replace("#undef __MSVCRT_VERSION__\n", "")
        .replace(pin, "// __MSVCRT_VERSION__ left to the toolchain default");
    // Tidy any blank lines left behind by the removal.
    while patched.contains("\n\n\n") {
        patched = patched.replace("\n\n\n", "\n\n");
    }
    println!(
        "Patching {} to use toolchain __MSVCRT_VERSION__ ...",
        path.display()
    );
    fs::write(path, patched).with_context(|| format!("failed to patch {}", path.display()))?;
    Ok(())
}

fn core_extension() -> &'static str {
    if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    }
}
