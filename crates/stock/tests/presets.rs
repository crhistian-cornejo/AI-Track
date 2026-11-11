//! Every factory preset parses, targets a real plugin, uses valid parameter
//! ids and normalised values, and renders finite audio.

mod harness;

use aitrack_stock::presets;
use harness::{BLOCK, Driver, SR};
use std::collections::BTreeMap;

#[test]
fn factory_presets_are_valid() {
    let all = presets::all();
    assert!(!all.is_empty(), "no factory presets embedded");
    let mut failures = Vec::new();
    for r in &all {
        let preset = match presets::load(&r.path) {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{}: {e}", r.path));
                continue;
            }
        };
        if preset.plugin != r.plugin {
            failures.push(format!("{}: plugin field {} does not match folder", r.path, preset.plugin));
        }
        if preset.name != r.name {
            failures.push(format!("{}: name field {:?} does not match file", r.path, preset.name));
        }
        let Some(mut p) = aitrack_stock::create(&preset.plugin) else {
            failures.push(format!("{}: unknown plugin {}", r.path, preset.plugin));
            continue;
        };
        let ids: Vec<String> = p.info().params.iter().map(|i| i.id.to_string()).collect();
        for (k, v) in &preset.params {
            if !ids.contains(k) {
                failures.push(format!("{}: unknown param id {k}", r.path));
            }
            if !(0.0..=1.0).contains(v) {
                failures.push(format!("{}: param {k} = {v} not normalised", r.path));
            }
        }
        if preset.tags.is_empty() {
            failures.push(format!("{}: no tags", r.path));
        }
        p.prepare(SR, BLOCK);
        preset.apply(p.as_mut());
        let mut d = Driver::new();
        for _ in 0..40 {
            d.block(p.as_mut(), 5);
            if let Err(e) = d.output_ok(1e3) {
                failures.push(format!("{}: {e}", r.path));
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn preset_counts() {
    let mut by_plugin: BTreeMap<String, usize> = BTreeMap::new();
    for r in presets::all() {
        *by_plugin.entry(r.plugin).or_default() += 1;
    }
    for entry in aitrack_stock::registry() {
        let n = by_plugin.get(entry.id).copied().unwrap_or(0);
        let min = match entry.id {
            "instrument.wavetable" => 60,
            "instrument.sampler" | "instrument.drum" | "fx.analyzer" => 1,
            _ => 6,
        };
        assert!(n >= min, "{} has {n} presets, want ≥ {min}", entry.id);
    }
}
