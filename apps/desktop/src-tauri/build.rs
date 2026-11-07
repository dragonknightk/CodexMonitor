#[allow(
    dead_code,
    reason = "the runtime consumes the remaining channel metadata"
)]
#[path = "src/build_channel.rs"]
mod build_channel;

fn main() {
    println!("cargo:rerun-if-env-changed={}", build_channel::ENVIRONMENT);
    let channel = std::env::var(build_channel::ENVIRONMENT).unwrap_or_else(|_| {
        build_channel::BuildChannel::LocalDevelopment
            .as_str()
            .to_owned()
    });
    let channel = build_channel::BuildChannel::parse(&channel)
        .unwrap_or_else(|| panic!("unknown CodexMonitor build channel"));
    println!(
        "cargo:rustc-env={}={}",
        build_channel::ENVIRONMENT,
        channel.as_str()
    );
    tauri_build::build();
}
