//! PulseTerm Remote -- the remote desktop helper PulseTerm launches.
//!
//! A separate program (GPL-3.0; see REMOTE_DESKTOP.md), built only on request
//! with `cargo build -p pulseterm-remote`. PulseTerm starts it with:
//!
//! ```text
//! pulseterm-remote --label user@host:22 --address host --theme '{...}'
//! ```
//!
//! and serves it over stdin/stdout (`bridge`). `--local` instead probes this
//! machine with no PulseTerm and no SSH -- for developing the launcher.
//!
//! Planned shape (REMOTE_DESKTOP.md, 2.7): this process is the gpui launcher;
//! `pulseterm-remote stream` will be the SDL3 stream window.

mod bridge;
mod config;
mod launcher;
mod probe;
mod sunshine;
mod theme;

use futures::StreamExt;
use gpui::{prelude::*, px, size, App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions};

use bridge::{Bridge, Event};
use launcher::Launcher;
use theme::Theme;

struct Args {
    label: String,
    address: String,
    theme: Option<String>,
    local: bool,
}

fn args() -> Args {
    let mut out = Args { label: String::new(), address: String::new(), theme: None, local: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--label" => out.label = it.next().unwrap_or_default(),
            "--address" => out.address = it.next().unwrap_or_default(),
            "--theme" => out.theme = it.next(),
            "--local" => out.local = true,
            other => eprintln!("pulseterm-remote: ignoring unknown argument {other:?}"),
        }
    }
    if out.local && out.label.is_empty() {
        out.label = "this computer".into();
        out.address = "localhost".into();
    }
    out
}

fn main() {
    let args = args();
    if args.label.is_empty() {
        eprintln!(
            "pulseterm-remote is started by PulseTerm (the remote desktop icon on an SSH session).\n\
             To try the launcher against this machine: pulseterm-remote --local"
        );
        std::process::exit(2);
    }

    let (bridge, events) = if args.local {
        (Bridge::local(), None)
    } else {
        let (bridge, events) = Bridge::pipe();
        (bridge, Some(events))
    };

    Application::new().run(move |cx: &mut App| {
        let mut theme = Theme::from_handoff(args.theme.as_deref());
        theme.resolve_font(&cx.text_system().all_font_names());
        cx.set_global(theme);
        let bounds = Bounds::centered(None, size(px(980.), px(640.)), cx);
        let (label, address) = (args.label.clone(), args.address.clone());
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some(format!("PulseTerm Remote — {label}").into()),
                        ..Default::default()
                    }),
                    app_id: Some("pulseterm-remote".into()),
                    ..Default::default()
                },
                move |_, cx| cx.new(|cx| Launcher::new(label, address, bridge, cx)),
            )
            .expect("failed to open window");

        // PulseTerm's side of the pipe: bring the window forward when the icon
        // is clicked again, and quit when PulseTerm lets go of us.
        if let Some(mut events) = events {
            cx.spawn(async move |cx| {
                while let Some(event) = events.next().await {
                    match event {
                        Event::Raise => {
                            let _ = window.update(cx, |_, window, _| window.activate_window());
                        }
                        Event::Closed => {
                            let _ = cx.update(|cx| cx.quit());
                            break;
                        }
                    }
                }
            })
            .detach();
        }
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
