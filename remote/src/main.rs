//! PulseTerm Remote -- the remote desktop helper PulseTerm launches.
//!
//! A placeholder until phase 1. Planned shape (REMOTE_DESKTOP.md, 2.7): one
//! binary, two processes -- `pulseterm-remote` is the gpui launcher (host
//! checks, config, connect) and `pulseterm-remote stream` the SDL3 window
//! (video, input, audio).

fn main() {
    eprintln!("pulseterm-remote: not built yet -- see REMOTE_DESKTOP.md");
    std::process::exit(1);
}
