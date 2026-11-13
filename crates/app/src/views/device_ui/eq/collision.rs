//! Where the EQ'd track and its sidechain fight for the same frequencies:
//! both loud and close in level. Pure maths for the EQ display.

/// Collision strength (0–1) at each sidechain-spectrum point. `post` is the
/// post-EQ spectrum and `sc` the sidechain's, both in dB over the same
/// log-spaced 20 Hz – 20 kHz span (any point counts).
pub fn collision(post: &[f32], sc: &[f32]) -> Vec<f32> {
    if post.len() < 2 || sc.len() < 2 {
        return Vec::new();
    }
    let last = (post.len() - 1) as f32 / (sc.len() - 1) as f32;
    sc.iter()
        .enumerate()
        .map(|(i, &b)| {
            let a = post[((i as f32 * last).round() as usize).min(post.len() - 1)];
            let loud = ((a.min(b) + 60.) / 36.).clamp(0., 1.);
            let close = (1. - (a - b).abs() / 18.).clamp(0., 1.);
            loud * close
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loud_overlapping_regions_collide() {
        let post: Vec<f32> = (0..128).map(|i| if i < 40 { -12. } else { -90. }).collect();
        let sc: Vec<f32> = (0..48).map(|i| if i < 15 { -14. } else if i < 30 { -12. } else { -90. }).collect();
        let c = collision(&post, &sc);
        assert_eq!(c.len(), 48);
        // Both loud and within 2 dB: strong.
        assert!(c[5] > 0.8, "{}", c[5]);
        // The track is gone there: nothing to collide with.
        assert!(c[20] < 0.05, "{}", c[20]);
        // Both silent.
        assert_eq!(c[40], 0.);
        assert!(collision(&post, &[]).is_empty());
    }

    #[test]
    fn a_big_level_gap_is_not_a_collision() {
        let post = vec![-6.; 128];
        let sc = vec![-40.; 48];
        assert!(collision(&post, &sc).iter().all(|v| *v < 0.01));
    }
}
