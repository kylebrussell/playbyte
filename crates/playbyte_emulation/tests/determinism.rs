use playbyte_emulation::EmulatorRuntime;
use sha1::{Digest, Sha1};

fn frame_hash(frame: &playbyte_libretro::VideoFrame) -> String {
    let mut hasher = Sha1::new();
    hasher.update(&frame.data);
    format!("{:x}", hasher.finalize())
}

#[test]
fn libretro_state_determinism() {
    let core = match std::env::var("PLAYBYTE_CORE_PATH") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("Skipping determinism test: PLAYBYTE_CORE_PATH not set");
            return;
        }
    };
    let rom = match std::env::var("PLAYBYTE_ROM_PATH") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("Skipping determinism test: PLAYBYTE_ROM_PATH not set");
            return;
        }
    };

    let mut runtime = EmulatorRuntime::new(core, rom).expect("runtime init failed");
    for _ in 0..30 {
        runtime.run_frame();
    }
    let state = runtime.serialize().expect("serialize failed");
    for _ in 0..10 {
        runtime.run_frame();
    }
    let frame_a = runtime.latest_frame().expect("missing frame A");
    let hash_a = frame_hash(&frame_a);

    runtime.unserialize(&state).expect("unserialize failed");
    for _ in 0..10 {
        runtime.run_frame();
    }
    let frame_b = runtime.latest_frame().expect("missing frame B");
    let hash_b = frame_hash(&frame_b);

    assert_eq!(hash_a, hash_b, "Frame hashes diverged after reload");
}
