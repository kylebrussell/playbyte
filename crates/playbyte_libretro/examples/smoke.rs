use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let core_path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: cargo run -p playbyte_libretro --example smoke <core> <rom>");
    let rom_path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: cargo run -p playbyte_libretro --example smoke <core> <rom>");

    let frame = playbyte_libretro::smoke_test(core_path, rom_path, 60).expect("smoke test failed");
    println!(
        "Captured frame {}x{} pitch={} format={:?}",
        frame.width, frame.height, frame.pitch, frame.pixel_format
    );
}
