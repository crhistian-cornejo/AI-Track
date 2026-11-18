//! `instrument.bloom`: the generative scene. The card is a smaller live
//! render of the window's matter (the GPU stage, `bloom/stage.rs`) with
//! Roll, Mutate, Deviance and the four macros; the expanded card adds the
//! species, the morph pad and the tools; the device window has everything,
//! the material picker and the DNA panel included ([`window`]).

use super::{Dev, kit};
use crate::icons::Icon;
use crate::theme::*;
use crate::widgets::{self, mono};
use crate::workspace::Workspace;
use aitrack_model::{Color, Command, Device, DeviceId, Track, TrackKind};
use aitrack_stock::instrument::bloom::genome::Species;
use aitrack_stock::instrument::bloom::{self as bloom, p};
use gpui::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px};

/// The device window's editor and everything the visual is made of.
#[path = "../device_window/bloom/mod.rs"]
pub mod window;

use window::{dial, stage, view};

pub fn render(ws: &mut Workspace, dev: &Dev, _d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    if kit::in_window() {
        return window::render(ws, dev, cx);
    }
    let pal = p();
    let scene = view::shown(ws, dev);
    let layout = card_layout(dev.big);
    let selected = dev.choice(p::SELECTED).min(15);
    // The same matter as the window, smaller and filling its square, in a
    // bezel its corners take the colour of.
    let bezel = pal.surface;
    let field = stage::stage(ws, dev, &scene, stage::Opts { swatches: false, outer: bezel, slot: 1, zoom: 1.2 }, cx);
    let sound = format!("{:02} {}", selected + 1, view::names(ws, dev, &scene)[selected]);
    let family = if view::kind(dev).is_tonal() { view::kind(dev).label() } else { scene.species.label() };
    let overlay = |text: String, color: gpui::Hsla| div().px(px(SP1)).h(px(16.)).flex().items_center().rounded(px(r_sm())).bg(with_alpha(pal.surface, 0.72)).child(mono(text, TEXT_XS, color));
    let well = div()
        .id(("bloom-card-stage", dev.key(0x700)))
        .relative()
        .flex_none()
        .w(px(layout.stage_w))
        .h_full()
        .p(px(SP1))
        .rounded(px(r_lg()))
        .bg(bezel)
        .border_1()
        .border_color(border())
        .tooltip(widgets::tip(window::TIP_STAGE, None))
        .child(field)
        .child(div().absolute().top(px(SP2)).left(px(SP2)).child(overlay(format!("{} · {:06}", family, bloom::seed_of(dev.plain(p::SEED))), pal.muted)))
        .child(
            div()
                .absolute()
                .bottom(px(SP2))
                .left(px(SP2))
                .flex()
                .gap(px(SP1))
                .child(overlay(sound, pal.text))
                .when(scene.locked(selected), |d| d.child(overlay("locked".into(), pal.accent))),
        );
    // Roll and Mutate, the history, and the way into the window.
    let d = dev.clone();
    let open = widgets::sized_icon_button(("bloom-open", dev.key(0x701)), Icon::OpenWindow, false, 24., 14.)
        .tooltip(widgets::tip(window::TIP_OPEN, None))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            crate::views::device_window::open(this, d.id, cx);
            cx.notify();
        }));
    let actions = div().flex_none().flex().items_center().gap(px(SP1)).children(window::grow_buttons(ws, dev, cx)).child(div().flex_1()).child(open);
    // As many knobs of the table as the rows hold, in its order.
    let has = |id: u32| dev.info(id).is_some();
    let mut grid = div().flex().flex_wrap().gap(px(SP1)).content_start();
    for k in window::knobs::first(layout.knobs, has) {
        grid = grid.child(dial(dev, k.id, KNOB, cx));
    }
    let side = div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(SP2)).child(actions).child(grid);
    kit::panel(layout.width).child(div().flex_1().min_h(px(0.)).flex().gap(px(SP2)).child(well).child(side)).into_any_element()
}

/// The card's knob size.
const KNOB: f32 = 24.;

/// How the card is laid out at its two widths.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardLayout {
    pub width: f32,
    pub stage_w: f32,
    /// How many knobs of the table it shows.
    pub knobs: usize,
}

/// The compact card (460 px) and the expanded one (720 px): the stage a
/// square-ish well on the left, the knobs in two rows (three expanded) beside it.
pub fn card_layout(big: bool) -> CardLayout {
    let (width, stage_w, rows) = if big { (720., 232., 3) } else { (460., 168., 2) };
    // The panel's padding, the gap to the stage.
    let side = width - 2. * SP2 - stage_w - SP2;
    CardLayout { width, stage_w, knobs: window::knobs::fit(side, window::dial_w(KNOB), SP1, rows) }
}

/// The dev-view words: `bloom` adds a Bloom on a new track with a seeded
/// scene (the Devices tab open), `bloomwin` returns it to open in its window,
/// `bloomplay` lights some cells with synthetic activity.
pub fn dev_word(ws: &mut Workspace, word: &str) -> Option<DeviceId> {
    let existing = ws.s.project.tracks.iter().flat_map(|t| &t.devices).find(|d| matches!(&d.kind, aitrack_model::DeviceKind::Stock { plugin_id } if plugin_id == bloom::ID)).map(|d| d.id);
    let id = match existing {
        Some(id) => id,
        None => {
            let track = ws.s.project.alloc_track_id();
            let device = ws.s.project.alloc_device_id();
            let mut t = Track::new(track, TrackKind::Instrument, "Bloom", Color::palette(6));
            let mut d = Device::stock(device, bloom::ID, "Bloom");
            let scene = bloom::grow::generate(271_828, bloom::grow::GrowOpts { species: Species::Mixed, spread: 1.0 });
            let params = bloom::params();
            let norm = |id: u32, v: f32| params.iter().find(|i| i.id == id).map_or(0., |i| i.to_normalized(v));
            d.params.insert(p::SEED, norm(p::SEED, scene.seed as f32));
            d.params.insert(p::SPECIES, norm(p::SPECIES, scene.species.index() as f32));
            d.params.insert(p::SELECTED, norm(p::SELECTED, 6.));
            d.state = Some(bloom::state_json(&d.params, &scene, &Default::default()));
            t.devices.push(d);
            let index = ws.s.project.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(ws.s.project.tracks.len());
            if !ws.s.edit("Add track", vec![Command::AddTrack { track: Box::new(t), index }]) {
                return None;
            }
            ws.s.selected_track = Some(track);
            device
        }
    };
    ws.s.bottom_tab = crate::state::BottomTab::Devices;
    if word == "bloomplay" {
        let v = ws.fx.bloom.view(id);
        let mut env = [0.0; 16];
        for (i, e) in [(0, 1.0), (2, 0.55), (6, 0.85), (10, 0.7), (13, 0.4)] {
            env[i] = e;
        }
        v.fake = Some(env);
        for (a, e) in v.anim.iter_mut().zip(env) {
            a.swell = e;
        }
        v.rings = vec![view::RingAnim { cell: 0, age: 0.12 }, view::RingAnim { cell: 6, age: 0.3 }, view::RingAnim { cell: 10, age: 0.05 }];
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_holds_the_macros_and_room_to_grow() {
        let small = card_layout(false);
        assert!(small.knobs >= 10, "{small:?}");
        let shown: Vec<u32> = window::knobs::first(small.knobs, |_| true).map(|k| k.id).collect();
        for id in [p::DECAY, p::BODY, p::TONE, p::GRIT, p::ATTACK, p::MOTION, p::FILTER, p::SPACE] {
            assert!(shown.contains(&id), "{id}");
        }
        // The expanded card has room for the whole table.
        let big = card_layout(true);
        assert!(big.knobs >= window::knobs::KNOBS.len(), "{big:?}");
        for v in [small.width, small.stage_w, big.width, big.stage_w, KNOB] {
            assert_eq!(v % 4., 0., "{v}");
        }
    }
}
