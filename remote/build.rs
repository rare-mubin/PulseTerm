//! Builds moonlight-common-c (the GameStream/Moonlight protocol, GPL-3.0,
//! vendored as a git submodule in `third_party/`) as a static library and
//! generates Rust bindings for its public header, `Limelight.h`.
//!
//! This only runs for `cargo build -p pulseterm-remote`: PulseTerm itself never
//! depends on this crate (see CLAUDE.md, Commands).
//!
//! Needs CMake, a C compiler, clang (for bindgen) and OpenSSL's headers.

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = root.join("third_party/moonlight-common-c");
    if !src.join("src/Limelight.h").exists() {
        panic!(
            "moonlight-common-c is missing: run `git submodule update --init --recursive` \
             in the PulseTerm repository"
        );
    }
    println!("cargo:rerun-if-changed=third_party/moonlight-common-c/src");
    println!("cargo:rerun-if-changed=build.rs");

    // The project has no install rule for itself, only for enet, so its
    // target is built directly and linked from CMake's build tree.
    let out = cmake::Config::new(&src)
        .define("BUILD_SHARED_LIBS", "OFF")
        // Upstream builds with -Werror; a newer compiler's new warning must
        // not break this build.
        .cflag("-Wno-error")
        .build_target("moonlight-common-c")
        .profile("Release")
        .build();
    let build = out.join("build");
    println!("cargo:rustc-link-search=native={}", build.display());
    println!("cargo:rustc-link-search=native={}", build.join("enet").display());
    println!("cargo:rustc-link-lib=static=moonlight-common-c");
    println!("cargo:rustc-link-lib=static=enet");
    println!("cargo:rustc-link-lib=crypto");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=ws2_32");
        println!("cargo:rustc-link-lib=winmm");
    }

    let bindings = bindgen::Builder::default()
        .header(src.join("src/Limelight.h").to_string_lossy())
        .allowlist_function("Li.*")
        .allowlist_type("_?(STREAM_CONFIGURATION|SERVER_INFORMATION|DECODE_UNIT|LENTRY|DECODER_RENDERER_CALLBACKS|AUDIO_RENDERER_CALLBACKS|CONNECTION_LISTENER_CALLBACKS|OPUS_MULTISTREAM_CONFIGURATION)")
        .allowlist_var("(STREAM_CFG|COLORSPACE|COLOR_RANGE|ENCFLG|BUFFER_TYPE|FRAME_TYPE|VIDEO_FORMAT|CAPABILITY|DR|STAGE|ML_ERROR|CONN_STATUS|SCM|BUTTON|KEY_ACTION|MODIFIER)_.*")
        .generate()
        .expect("bindgen could not read Limelight.h");
    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("limelight.rs");
    bindings.write_to_file(dest).expect("could not write the bindings");
}
