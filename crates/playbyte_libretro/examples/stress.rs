use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let core_path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: cargo run -p playbyte_libretro --example stress <core> <rom> [frames]");
    let rom_path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: cargo run -p playbyte_libretro --example stress <core> <rom> [frames]");
    let frames: usize = args
        .next()
        .as_deref()
        .unwrap_or("6000")
        .parse()
        .expect("frames must be an integer");

    let frame = playbyte_libretro::smoke_test(core_path, rom_path, frames).expect("stress failed");
    println!(
        "Captured frame after {frames} frames: {}x{} pitch={} format={:?}",
        frame.width, frame.height, frame.pitch, frame.pixel_format
    );
}
