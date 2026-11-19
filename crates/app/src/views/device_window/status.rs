//! The window's footer: what a plugin shows along the bottom. Latency the
//! device declares, the DSP load, and a warning while the device is working
//! too hard or bypassed.
//!
//! Plugins that publish their latency in their meter ([`super::io`]) are
//! read from it; for the other effects the latency comes from a private
//! instance of the processor that is set to the device's parameters (the
//! same number the engine's delay compensation uses), kept in [`Probe`].

use super::io::{self, Reading};
use super::placement::FOOTER_H;
use crate::theme::*;
use crate::views::device_ui::{Dev, ParamSet};
use crate::widgets::{self, label, mono};
use crate::workspace::Workspace;
use aitrack_dsp::Processor;
use gpui::{AnyElement, FontWeight, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder, px};
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

/// A private processor that answers "how much latency with these settings".
#[derive(Default)]
pub struct Probe {
    plugin: String,
    proc: Option<Box<dyn Processor>>,
    key: u64,
    samples: u32,
}

impl Probe {
    /// The latency in samples of `plugin` set to `values` (stored values;
    /// missing = default) at `sample_rate`. Recomputed only when they change.
    pub fn latency(&mut self, plugin: &str, set: &ParamSet, values: &BTreeMap<u32, f32>, sample_rate: f64) -> u32 {
        // Instruments and MIDI effects add no audio latency worth a number here.
        if !plugin.starts_with("fx.") {
            return 0;
        }
        let key = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            plugin.hash(&mut h);
            sample_rate.to_bits().hash(&mut h);
            for (id, v) in values {
                (id, v.to_bits()).hash(&mut h);
            }
            h.finish()
        };
        if self.plugin != plugin {
            self.plugin = plugin.to_owned();
            self.proc = aitrack_stock::create(plugin).map(|mut p| {
                p.prepare(sample_rate, 512);
                p
            });
            self.key = 0;
        }
        let Some(proc) = self.proc.as_mut() else { return 0 };
        if key != self.key {
            self.key = key;
            for info in &set.list {
                proc.set_param(info.id, values.get(&info.id).copied().unwrap_or_else(|| info.default_normalized()));
            }
            self.samples = proc.latency();
        }
        self.samples
    }
}

/// What the footer says about the device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notes {
    pub latency: Option<u32>,
    pub warning: Option<String>,
}

/// The latency and the warning for the footer.
pub fn notes(ws: &mut Workspace, dev: &Dev, plugin: &str, reading: &Reading) -> Notes {
    let latency = reading.latency.or_else(|| {
        let values = ws.s.project.device(dev.id).map(|d| d.params.clone())?;
        let probe = &mut ws.fx.chrome.entry(dev.id).or_default().probe;
        Some(probe.latency(plugin, &dev.set, &values, dev.sample_rate)).filter(|n| *n > 0)
    });
    let warning = reading.warn.map(str::to_owned).or_else(|| ws.b.engine.device_warning(dev.id));
    Notes { latency, warning }
}

/// The footer.
pub fn footer(ws: &Workspace, dev: &Dev, notes: &Notes) -> AnyElement {
    let pal = p();
    let cpu = ws.b.engine.cpu_load().clamp(0., 1.);
    let item = |name: &str, value: String, color: gpui::Hsla| {
        div().flex_none().flex().items_center().gap(px(SP1)).child(label(name.to_owned(), DEV_AXIS, pal.faint)).child(mono(value, DEV_AXIS, color))
    };
    div()
        .flex_none()
        .h(px(FOOTER_H))
        .px(px(SP4))
        .gap(px(SP4))
        .flex()
        .items_center()
        .bg(pal.panel)
        .border_t_1()
        .border_color(border())
        .child(match notes.latency {
            Some(n) => item("LATENCY", io::latency_text(n, dev.sample_rate), pal.muted),
            None => item("LATENCY", "none".to_owned(), pal.faint),
        })
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(label("DSP", DEV_AXIS, pal.faint))
                .child(widgets::hbar(cpu, 48., 4., if cpu > 0.85 { pal.record } else if cpu > 0.6 { pal.meter_mid } else { pal.muted }))
                .child(mono(format!("{:.0}%", cpu * 100.), DEV_AXIS, pal.muted)),
        )
        .child(div().flex_1())
        .when_some(notes.warning.clone(), |d, w| {
            d.child(
                div()
                    .flex_none()
                    .h(px(18.))
                    .px(px(SP2))
                    .flex()
                    .items_center()
                    .rounded(px(r_sm()))
                    .bg(with_alpha(pal.record, 0.16))
                    .child(label(w, DEV_AXIS, pal.record).font_weight(FontWeight::SEMIBOLD)),
            )
        })
        .when(!dev.enabled, |d| {
            d.child(
                div()
                    .flex_none()
                    .h(px(18.))
                    .px(px(SP2))
                    .flex()
                    .items_center()
                    .rounded(px(r_sm()))
                    .bg(with_alpha(pal.mute, 0.18))
                    .child(label("Bypassed: the signal passes untouched", DEV_AXIS, pal.mute).font_weight(FontWeight::SEMIBOLD)),
            )
        })
        .child(item("RATE", format!("{:.1} kHz", dev.sample_rate / 1000.), pal.faint))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{PluginCatalog, StubCatalog};

    fn set_of(plugin: &str) -> ParamSet {
        ParamSet::new(StubCatalog::default().device(plugin).unwrap().params.clone())
    }

    #[test]
    fn the_probe_follows_the_settings_that_make_latency() {
        let mut probe = Probe::default();
        let comp = set_of("fx.compressor");
        let look = aitrack_stock::fx::compressor::p::LOOKAHEAD;
        let info = comp.get(look).unwrap();
        assert_eq!(probe.latency("fx.compressor", &comp, &BTreeMap::new(), 48_000.), 0, "no look-ahead by default");
        let on: BTreeMap<u32, f32> = [(look, info.to_normalized(info.max))].into();
        let samples = probe.latency("fx.compressor", &comp, &on, 48_000.);
        assert!(samples > 0, "look-ahead is latency");
        // The same settings again do not recompute (and give the same number).
        assert_eq!(probe.latency("fx.compressor", &comp, &on, 48_000.), samples);
        // Switching plugin starts again.
        assert_eq!(probe.latency("instrument.wavetable", &set_of("instrument.wavetable"), &BTreeMap::new(), 48_000.), 0);
        assert_eq!(probe.latency("fx.nothing", &comp, &BTreeMap::new(), 48_000.), 0);
    }
}
