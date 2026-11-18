//! The materials the mass can be made of. Each is a set of uniforms; the
//! sound's own look (`mapping.rs`) modulates on top of it.

/// A material of the Bloom visual (stored by name in the device state).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Material {
    /// Black ferrofluid: wet obsidian with a faint oil sheen.
    #[default]
    Obsidian,
    /// Liquid chrome.
    Mercury,
    /// Black with a strong thin-film rainbow.
    OilSlick,
    /// White nacre.
    Pearl,
    /// Dark crust with something molten in the creases.
    Ember,
    /// Clear resin.
    Glass,
    /// Matte black with heavy vapour.
    Ink,
}

/// What a material sets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialParams {
    /// Linear albedo (the dielectric's body colour, or the metal's F0).
    pub albedo: [f32; 3],
    pub metal: f32,
    pub roughness: f32,
    /// Strength of the reflections (1 = physically plausible).
    pub clearcoat: f32,
    /// 0..1: how much the thin film colours the reflections.
    pub iridescence: f32,
    /// Film thickness and its spread over the surface, µm.
    pub film: f32,
    pub film_range: f32,
    pub rim: f32,
    /// 0..1: how much the body lets the studio through.
    pub glass: f32,
    pub emission: [f32; 3],
    pub emission_amount: f32,
    /// Diffuse wrap (soft, subsurface-looking light for the pale materials).
    pub wrap: f32,
    pub exposure: f32,
    /// Multipliers of the sound's smoke, lumps and spikes.
    pub smoke: f32,
    pub lumps: f32,
    pub spikes: f32,
    /// 0..1: a warmer key light.
    pub warmth: f32,
}

impl Material {
    pub const ALL: [Material; 7] = [Material::Obsidian, Material::Mercury, Material::OilSlick, Material::Pearl, Material::Ember, Material::Glass, Material::Ink];

    pub fn name(self) -> &'static str {
        match self {
            Material::Obsidian => "Obsidian",
            Material::Mercury => "Mercury",
            Material::OilSlick => "Oil Slick",
            Material::Pearl => "Pearl",
            Material::Ember => "Ember",
            Material::Glass => "Glass",
            Material::Ink => "Ink",
        }
    }

    /// The key stored in the device state.
    pub fn key(self) -> &'static str {
        match self {
            Material::Obsidian => "obsidian",
            Material::Mercury => "mercury",
            Material::OilSlick => "oil",
            Material::Pearl => "pearl",
            Material::Ember => "ember",
            Material::Glass => "glass",
            Material::Ink => "ink",
        }
    }

    pub fn from_key(key: &str) -> Option<Material> {
        Material::ALL.into_iter().find(|m| m.key() == key)
    }

    pub fn index(self) -> usize {
        Material::ALL.iter().position(|m| *m == self).unwrap_or(0)
    }

    /// The next (`dir` = 1) or previous (−1) material, wrapping.
    pub fn step(self, dir: i32) -> Material {
        let n = Material::ALL.len() as i32;
        Material::ALL[((self.index() as i32 + dir).rem_euclid(n)) as usize]
    }

    pub fn params(self) -> MaterialParams {
        let base = MaterialParams {
            albedo: [0.006, 0.006, 0.007],
            metal: 0.0,
            roughness: 0.04,
            clearcoat: 1.0,
            iridescence: 0.38,
            film: 0.42,
            film_range: 0.30,
            rim: 0.30,
            glass: 0.0,
            emission: [0.0; 3],
            emission_amount: 0.0,
            wrap: 0.0,
            exposure: 1.0,
            smoke: 1.0,
            lumps: 1.0,
            spikes: 1.0,
            warmth: 0.0,
        };
        match self {
            Material::Obsidian => base,
            Material::Mercury => MaterialParams { albedo: [0.80, 0.81, 0.83], metal: 1.0, roughness: 0.03, iridescence: 0.12, rim: 0.12, smoke: 0.6, film: 0.30, ..base },
            Material::OilSlick => MaterialParams { albedo: [0.004, 0.004, 0.006], iridescence: 1.0, film: 0.48, film_range: 0.55, rim: 0.4, ..base },
            Material::Pearl => {
                MaterialParams { albedo: [0.72, 0.70, 0.68], roughness: 0.12, clearcoat: 0.8, iridescence: 0.55, film: 0.30, film_range: 0.25, rim: 0.18, wrap: 0.7, exposure: 0.85, smoke: 0.7, warmth: 0.3, ..base }
            }
            Material::Ember => MaterialParams {
                albedo: [0.012, 0.008, 0.006],
                roughness: 0.16,
                clearcoat: 0.75,
                iridescence: 0.12,
                rim: 0.18,
                emission: [1.0, 0.32, 0.06],
                emission_amount: 0.85,
                smoke: 1.2,
                warmth: 0.8,
                ..base
            },
            Material::Glass => MaterialParams { albedo: [0.55, 0.72, 0.80], roughness: 0.02, iridescence: 0.25, rim: 0.45, glass: 1.0, smoke: 0.7, lumps: 0.8, ..base },
            Material::Ink => MaterialParams { albedo: [0.010, 0.010, 0.012], roughness: 0.42, clearcoat: 0.55, iridescence: 0.18, rim: 0.12, smoke: 2.2, lumps: 1.25, spikes: 0.7, ..base },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_material_has_a_unique_key_and_sane_values() {
        let mut keys = std::collections::HashSet::new();
        for m in Material::ALL {
            assert!(keys.insert(m.key()), "{:?}", m);
            assert_eq!(Material::from_key(m.key()), Some(m));
            assert_eq!(Material::ALL[m.index()], m);
            let p = m.params();
            for v in [p.metal, p.roughness, p.iridescence, p.glass, p.wrap, p.warmth] {
                assert!((0.0..=1.0).contains(&v), "{m:?}");
            }
            assert!(p.albedo.iter().chain(&p.emission).all(|c| (0.0..=1.0).contains(c)));
            assert!(p.film > 0.1 && p.film < 1.2 && p.exposure > 0.3 && p.clearcoat > 0.2);
        }
        assert_eq!(Material::from_key("nope"), None);
        assert_eq!(Material::Obsidian.step(-1), Material::Ink);
        assert_eq!(Material::Ink.step(1), Material::Obsidian);
    }

    #[test]
    fn the_materials_differ_where_they_say() {
        let p = |m: Material| m.params();
        assert!(p(Material::Mercury).metal > 0.9, "chrome is a metal");
        assert!(p(Material::OilSlick).iridescence > p(Material::Obsidian).iridescence * 2.0);
        assert!(p(Material::Pearl).albedo[0] > 0.5 && p(Material::Obsidian).albedo[0] < 0.02, "pearl is white, obsidian black");
        assert!(p(Material::Ember).emission_amount > 0.0 && p(Material::Obsidian).emission_amount == 0.0);
        assert!(p(Material::Glass).glass > 0.9);
        assert!(p(Material::Ink).roughness > p(Material::Obsidian).roughness * 4.0 && p(Material::Ink).smoke > 1.5);
    }
}
