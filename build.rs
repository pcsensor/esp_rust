fn main() {
    // The esp-hal `linkall.x` linker script is only valid for the ESP firmware
    // link. Host builds (e.g. `cargo test` on macOS/Linux) use the system
    // linker, which rejects `-Tlinkall.x`, so only emit it for the bare-metal
    // target where `CARGO_CFG_TARGET_OS` is "none".
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none") {
        println!("cargo:rustc-link-arg=-Tlinkall.x");
    }
}
