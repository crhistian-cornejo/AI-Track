//! `instrument.bloom`: the generative scene. The card is the field (sixteen
//! cells whose colour and shape come from their sound, glowing as they
//! play) with Roll, Mutate, Deviance and the four macros; the expanded card
//! adds the species, the morph pad and the tools; the device window has
//! everything, the DNA panel included ([`window`]).

use super::{Dev, caption, kit};
use crate::theme::*;
use crate::widgets::{label, mono};
use crate::workspace::Workspace;
use aitrack_model::{Color, Command, Device, DeviceId, Track, TrackKind};
use aitrack_stock::instrument::bloom::genome::Species;
use aitrack_stock::instrument::bloom::{self as bloom, p};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};

/// The device window's editor and everything the visual is made of.
#[path = "../device_window/bloom/mod.rs"]
pub mod window;

use window::{dial, view, visual};

pub fn render(ws: &mut Workspace, dev: &Dev, _d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    if kit::in_window() {
        return window::render(ws, dev, cx);
    }
    let pal = p();
    let scene = view::shown(ws, dev);
    let w = if dev.big { 720. } else { 460. };
    let selected = dev.choice(p::SELECTED).min(15);
    let field = visual::visual(ws, dev, &scene, dev.big, cx);
    let mut side = div().flex_none().w(px(112.)).flex().flex_col().gap(px(SP1));
    let buttons = window::grow_buttons(ws, dev, cx);
    let mut it = buttons.into_iter();
    let (roll, mutate) = (it.next(), it.next());
    side = side.children(roll).children(mutate).child(div().flex().gap(px(SP1)).children(it)).child(kit::num_w(ws, dev, p::DEVIANCE, 112., cx));
    let header = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(caption("BLOOM"))
        .child(label(scene.species.label(), TEXT_XS, pal.muted))
        .child(div().flex_1())
        .child(mono(format!("{} · {:02} {}", if scene.locked(selected) { "locked" } else { "sound" }, selected + 1, scene.sounds[selected].role.label()), TEXT_XS, pal.muted))
        .child(kit::readout("SEED", format!("{:06}", scene.seed), pal.accent));
    let mut macros = div().flex_none().flex().items_end().gap(px(SP1));
    for id in [p::DECAY, p::BODY, p::TONE, p::GRIT] {
        macros = macros.child(dial(dev, id, 26., cx));
    }
    let mut panel = kit::panel(w)
        .child(header)
        .child(div().flex_1().min_h(px(96.)).flex().gap(px(SP2)).child(div().flex_1().min_w(px(0.)).h_full().child(field)).child(side));
    if dev.big {
        macros = macros.child(dial(dev, p::SPACE, 26., cx)).child(dial(dev, p::VOLUME, 26., cx));
        let morph = window::morph(ws, dev, 64., cx);
        let tools = window::tools(ws, dev, cx);
        panel = panel
            .child(div().flex_none().flex().items_end().gap(px(SP3)).child(macros).child(morph).child(div().flex_1()).child(tools))
            .child(window::species_chips(dev, &scene, cx));
    } else {
        panel = panel.child(macros);
    }
    panel.into_any_element()
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
