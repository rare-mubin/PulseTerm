//! The launcher: what the host can do, how to stream it, and the way in.
//!
//! Drawn in PulseTerm's design language with plain gpui -- one bar, hairlines,
//! no radius, no shadow, tracked labels, segmented choices -- from the tokens
//! PulseTerm handed over (`theme`). It is the part of the helper that is
//! *before* streaming; the stream itself is an SDL3 window of its own
//! (REMOTE_DESKTOP.md, 2.7), not built yet.

use gpui::{
    div, prelude::*, px, ClickEvent, ClipboardItem, Context, Hsla, IntoElement, SharedString,
    Window,
};

use crate::bridge::Bridge;
use crate::config::{self, StreamConfig};
use crate::probe::{self, Check, Facts, State};
use crate::sunshine::{self, Instance};
use crate::theme::{label, Theme};

pub struct Launcher {
    host: String,
    address: String,
    bridge: Bridge,
    probe: Probe,
    config: StreamConfig,
    sunshine: Sun,
    /// One line under the actions: what the last action did, or why not.
    notice: Option<String>,
}

/// PulseTerm's own Sunshine instance on the host.
enum Sun {
    Stopped,
    Starting,
    Running(Instance),
    Failed(String),
}

enum Probe {
    Checking,
    Done(Facts),
    Failed(String),
}

impl Launcher {
    pub fn new(host: String, address: String, bridge: Bridge, cx: &mut Context<Self>) -> Self {
        let config = config::load(&host);
        let mut this = Self {
            host,
            address,
            bridge,
            probe: Probe::Checking,
            config,
            sunshine: Sun::Stopped,
            notice: None,
        };
        this.recheck(cx);
        this
    }

    fn recheck(&mut self, cx: &mut Context<Self>) {
        self.probe = Probe::Checking;
        self.notice = None;
        let bridge = self.bridge.clone();
        cx.spawn(async move |this, cx| {
            let result = bridge.exec(probe::SCRIPT).await;
            let _ = this.update(cx, |this, cx| {
                this.probe = match result {
                    Ok(out) => Probe::Done(Facts::parse(&out.stdout)),
                    Err(e) => Probe::Failed(e),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut StreamConfig)) {
        change(&mut self.config);
        config::save(&self.host, &self.config);
        cx.notify();
    }

    fn start_sunshine(&mut self, cx: &mut Context<Self>) {
        let Probe::Done(facts) = &self.probe else { return };
        if matches!(self.sunshine, Sun::Starting | Sun::Running(_)) {
            return;
        }
        let port = sunshine::base_port(facts);
        let kms = facts.kms;
        self.sunshine = Sun::Starting;
        let bridge = self.bridge.clone();
        cx.spawn(async move |this, cx| {
            let result = sunshine::start(&bridge, port, kms).await;
            let _ = this.update(cx, |this, cx| {
                this.sunshine = match result {
                    Ok(instance) => {
                        // Honest about where the work stands: pairing and the
                        // SDL3 stream window (phases 2 and 3) come next.
                        this.notice = Some(format!(
                            "Sunshine is up on port {}. Pairing and the stream window come next.",
                            instance.port
                        ));
                        Sun::Running(instance)
                    }
                    Err(e) => Sun::Failed(e),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn stop_sunshine(&mut self, cx: &mut Context<Self>) {
        let Sun::Running(instance) = self.sunshine else { return };
        self.sunshine = Sun::Stopped;
        self.notice = None;
        let bridge = self.bridge.clone();
        cx.spawn(async move |_, _| sunshine::stop(&bridge, instance).await).detach();
        cx.notify();
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        self.start_sunshine(cx);
    }
}

impl Render for Launcher {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        let checks = match &self.probe {
            Probe::Done(facts) => probe::checks(facts),
            _ => Vec::new(),
        };
        let (word, tint) = match &self.probe {
            Probe::Checking => ("checking", t.warning),
            Probe::Failed(_) => ("offline", t.danger),
            Probe::Done(_) if probe::ready(&checks) => ("ready", t.success),
            Probe::Done(_) => ("not ready", t.danger),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.fg)
            .font_family(SharedString::from(t.font.clone()))
            .text_size(t.font_size)
            .line_height(t.line_height)
            .child(self.bar(&t, word, tint))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.host_panel(&t, &checks, cx))
                    .child(self.config_panel(&t, cx)),
            )
            .child(self.footer(&t, probe::ready(&checks) && matches!(self.probe, Probe::Done(_)), cx))
    }
}

impl Launcher {
    /// The design's one bar: identity, the host, its state.
    fn bar(&self, t: &Theme, word: &str, tint: Hsla) -> impl IntoElement {
        div()
            .h(t.bar())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(16.))
            .px(px(16.))
            .bg(t.surface)
            .border_b_1()
            .border_color(t.border)
            .child(micro(t, "remote", t.fg_muted))
            .child(div().w(px(1.)).h(t.bar() * 0.6).bg(t.border))
            .child(div().child(self.host.clone()))
            .child(micro(t, word, tint))
            .child(div().flex_1())
            .child(div().text_color(t.fg_subtle).text_size(t.micro()).child(self.address.clone()))
    }

    fn host_panel(&self, t: &Theme, checks: &[Check], cx: &mut Context<Self>) -> impl IntoElement {
        let mut panel = div()
            .id("host")
            .flex_1()
            .min_w_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(20.))
            .border_r_1()
            .border_color(t.border)
            .child(heading(t, "Host", None));

        match &self.probe {
            Probe::Checking => {
                panel = panel.child(div().text_color(t.fg_muted).child("asking the host over SSH…"));
            }
            Probe::Failed(e) => {
                panel = panel.child(div().text_color(t.danger).child(format!("could not ask the host: {e}")));
            }
            Probe::Done(f) => {
                let gpu = match (&f.nvidia, f.gpus.is_empty()) {
                    (Some(n), _) => n.clone(),
                    (None, false) => f.gpus.join(" · "),
                    (None, true) => "unknown".into(),
                };
                panel = panel
                    .child(fact(t, "system", if f.distro.is_empty() { "unknown" } else { &f.distro }))
                    .child(fact(t, "name", &f.hostname))
                    .child(fact(t, "gpu", &gpu))
                    .child(div().h(px(1.)).bg(t.border))
                    .child(heading(t, "Checks", Some(format!("{}", checks.len()))));
                for (ix, check) in checks.iter().enumerate() {
                    panel = panel.child(check_row(t, ix, check, cx));
                }
                panel = panel.child(div().h(px(1.)).bg(t.border)).child(self.sunshine_row(t, cx));
            }
        }
        panel
    }

    fn config_panel(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let c = &self.config;
        let fps = c.fps;
        let bitrate = c.bitrate;
        div()
            .id("config")
            .w(px(400.))
            .flex_none()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(16.))
            .p(px(20.))
            .child(heading(t, "Stream", None))
            .child(setting(
                t,
                "Resolution",
                segmented(
                    t,
                    "res",
                    &[("window", "window"), ("native", "native"), ("1080", "1080"), ("1440", "1440"), ("2160", "4k")],
                    &c.resolution,
                    cx,
                    |this, v, cx| this.set(cx, |c| c.resolution = v.into()),
                ),
            ))
            .child(setting(
                t,
                "Frame rate",
                segmented(
                    t,
                    "fps",
                    &[("30", "30"), ("60", "60"), ("90", "90"), ("120", "120")],
                    &fps.to_string(),
                    cx,
                    |this, v, cx| this.set(cx, |c| c.fps = v.parse().unwrap_or(60)),
                ),
            ))
            .child(setting(
                t,
                "Codec",
                segmented(
                    t,
                    "codec",
                    &[("auto", "auto"), ("h264", "h.264"), ("hevc", "hevc"), ("av1", "av1")],
                    &c.codec,
                    cx,
                    |this, v, cx| this.set(cx, |c| c.codec = v.into()),
                ),
            ))
            .child(setting(
                t,
                "Bitrate, Mbps",
                segmented(
                    t,
                    "rate",
                    &[("0", "auto"), ("20", "20"), ("50", "50"), ("100", "100"), ("150", "150")],
                    &bitrate.to_string(),
                    cx,
                    |this, v, cx| this.set(cx, |c| c.bitrate = v.parse().unwrap_or(0)),
                ),
            ))
            .child(setting(t, "Audio", toggle(t, "audio", c.audio, cx, |c| &mut c.audio)))
            .child(setting(t, "Gamepads", toggle(t, "pads", c.gamepads, cx, |c| &mut c.gamepads)))
    }

    /// PulseTerm's own instance: what it is doing, and the one action that
    /// makes sense from there.
    fn sunshine_row(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let (word, tint, detail) = match &self.sunshine {
            Sun::Stopped => ("stopped", t.fg_subtle, "starts with connect, stops when this window closes".to_string()),
            Sun::Starting => (
                "starting",
                t.warning,
                "the first time, the host asks to allow screen sharing -- accept it on the host's screen".to_string(),
            ),
            Sun::Running(i) => ("running", t.success, format!("base port {}, its own config, nothing saved", i.port)),
            Sun::Failed(e) => ("failed", t.danger, e.clone()),
        };
        let action = match &self.sunshine {
            Sun::Running(_) => Some(("stop", true)),
            Sun::Stopped | Sun::Failed(_) => Some(("start", false)),
            Sun::Starting => None,
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(heading(t, "Sunshine", None))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .child(div().flex_none().text_color(tint).child("■"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().flex().gap(px(10.)).child("PulseTerm's instance").child(micro(t, word, tint)))
                            .child(div().text_color(t.fg_muted).child(detail)),
                    )
                    .children(action.map(|(text, stop)| {
                        button(t, "sun", text, t.fg_muted, t.bg, t.border).on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                if stop {
                                    this.stop_sunshine(cx)
                                } else {
                                    this.start_sunshine(cx)
                                }
                            },
                        ))
                    })),
            )
    }

    fn footer(&self, t: &Theme, ready: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let (fg, bg, line) = if ready { (t.bg, t.accent, t.accent) } else { (t.fg_subtle, t.bg, t.border) };
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(20.))
            .h(t.bar())
            .border_t_1()
            .border_color(t.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(t.fg_muted)
                    .child(self.notice.clone().unwrap_or_default()),
            )
            .child(
                button(t, "recheck", "recheck", t.fg_muted, t.bg, t.border)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.recheck(cx))),
            )
            .child(
                button(t, "connect", "connect", fg, bg, line)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if ready {
                            this.connect(cx);
                        } else {
                            this.notice = Some("Fix the checks marked in red first.".into());
                            cx.notify();
                        }
                    })),
            )
    }
}

fn micro(t: &Theme, text: &str, tint: Hsla) -> impl IntoElement {
    div().flex_none().text_size(t.micro()).text_color(tint).child(label(text))
}

fn heading(t: &Theme, text: &str, count: Option<String>) -> impl IntoElement {
    div()
        .flex()
        .items_baseline()
        .justify_between()
        .child(div().text_size(t.head()).font_weight(gpui::FontWeight::BOLD).child(text.to_uppercase()))
        .children(count.map(|c| micro(t, &c, t.fg_subtle)))
}

fn fact(t: &Theme, key: &str, value: &str) -> impl IntoElement {
    div()
        .flex()
        .gap(px(12.))
        .child(div().w(px(72.)).flex_none().pt(px(2.)).child(micro(t, key, t.fg_subtle)))
        .child(div().min_w_0().child(value.to_string()))
}

fn check_row(t: &Theme, ix: usize, check: &Check, cx: &mut Context<Launcher>) -> impl IntoElement {
    let tint = match check.state {
        State::Ok => t.success,
        State::Warn => t.warning,
        State::Fail => t.danger,
    };
    let mut row = div()
        .flex()
        .items_start()
        .gap(px(10.))
        .child(div().flex_none().text_color(tint).child("■"))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(check.label)
                .child(div().text_color(t.fg_muted).child(check.detail.clone())),
        );
    if let Some(fix) = check.fix.clone() {
        let shown = fix.clone();
        // Click to copy: the fix is run on the host, in a terminal PulseTerm
        // already has open there.
        row = row.child(
            div()
                .id(("fix", ix))
                .flex_none()
                .max_w(px(220.))
                .h(t.row())
                .flex()
                .items_center()
                .px(px(8.))
                .border_1()
                .border_color(t.border)
                .text_size(t.micro())
                .text_color(t.fg_muted)
                .hover(|s| s.border_color(t.accent))
                .child(div().min_w_0().truncate().child(format!("$ {shown}")))
                .tooltip(move |_, cx| {
                    let text: SharedString = format!("copy: {fix}").into();
                    cx.new(|_| Tip(text)).into()
                })
                .on_click(cx.listener({
                    let fix = check.fix.clone().unwrap_or_default();
                    move |this, _: &ClickEvent, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(fix.clone()));
                        this.notice = Some("Copied. Run it in a terminal on the host.".into());
                        cx.notify();
                    }
                })),
        );
    }
    row
}

/// A plain tooltip in the design's hand.
struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>();
        div()
            .max_w(px(480.))
            .px(px(8.))
            .py(px(4.))
            .bg(t.raised)
            .border_1()
            .border_color(t.border)
            .text_color(t.fg)
            .text_size(t.micro())
            .font_family(SharedString::from(t.font.clone()))
            .child(self.0.clone())
    }
}

fn setting(t: &Theme, name: &str, control: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(div().text_color(t.fg_muted).child(name.to_string()))
        .child(control)
}

/// The design's segmented choice: boxes in a row, the chosen one filled. Each
/// box gives up the edge it shares with the one before, so no edge is drawn
/// twice or painted over.
fn segmented(
    t: &Theme,
    id: &'static str,
    options: &[(&'static str, &'static str)],
    current: &str,
    cx: &mut Context<Launcher>,
    pick: fn(&mut Launcher, &str, &mut Context<Launcher>),
) -> impl IntoElement {
    let mut row = div().flex();
    for (ix, (value, text)) in options.iter().enumerate() {
        let chosen = *value == current;
        let value = *value;
        row = row.child(
            div()
                .id((id, ix))
                .px(px(10.))
                .h(t.row())
                .flex()
                .items_center()
                .border_1()
                .when(ix > 0, |d| d.border_l_0())
                .border_color(t.border)
                .text_size(t.micro())
                .when(chosen, |d| d.bg(t.fg).text_color(t.bg))
                .when(!chosen, |d| d.text_color(t.fg_muted).hover(|s| s.bg(t.raised)))
                .child(label(text))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| pick(this, value, cx))),
        );
    }
    row
}

fn toggle(
    t: &Theme,
    id: &'static str,
    on: bool,
    cx: &mut Context<Launcher>,
    field: fn(&mut StreamConfig) -> &mut bool,
) -> impl IntoElement {
    let mut row = div().flex();
    for (ix, (value, text)) in [(true, "on"), (false, "off")].into_iter().enumerate() {
        let chosen = value == on;
        row = row.child(
            div()
                .id((id, ix))
                .px(px(10.))
                .h(t.row())
                .flex()
                .items_center()
                .border_1()
                .when(ix > 0, |d| d.border_l_0())
                .border_color(t.border)
                .text_size(t.micro())
                .when(chosen, |d| d.bg(t.fg).text_color(t.bg))
                .when(!chosen, |d| d.text_color(t.fg_muted).hover(|s| s.bg(t.raised)))
                .child(label(text))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set(cx, |c| *field(c) = value)
                })),
        );
    }
    row
}

fn button(
    t: &Theme,
    id: &'static str,
    text: &str,
    fg: Hsla,
    bg: Hsla,
    line: Hsla,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px(px(14.))
        .h(t.row())
        .flex()
        .items_center()
        .border_1()
        .border_color(line)
        .bg(bg)
        .text_color(fg)
        .text_size(t.micro())
        .hover(|s| s.border_color(t.accent))
        .child(label(text))
}
