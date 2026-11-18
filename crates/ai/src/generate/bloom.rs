//! Matching a sample with Bloom: search the genome space for a sound that
//! sounds like a target recording, locally.
//!
//! The distance is the DSP timbre embedding ([`crate::embedding`], 64 values
//! of MFCC, spectral shape and envelope, gain-invariant): a candidate genome
//! is rendered offline as long as the target, described the same way, and
//! scored by the Euclidean distance between the two vectors.
//!
//! The search is a (μ + λ) evolution strategy in normalised gene space:
//!
//! 1. **Seeding**: the cores of every species' scene (and, when given, the
//!    sound in the cell being matched) are scored, so the search starts from
//!    the right family (a kick is not looked for among hats).
//! 2. **Generations**: λ children per generation, each from a parent drawn
//!    by rank among the μ best (some by uniform crossover of two parents),
//!    mutated by a normal step per continuous gene whose size shrinks over
//!    the run, and a small chance to switch each categorical gene. The μ
//!    best of parents and children survive (elitist: the best never gets
//!    worse).
//! 3. Candidates of a generation are rendered and scored on worker threads.
//!
//! Every random draw comes from the search seed and every evaluation is a
//! pure function of the genome, so a search is reproducible whatever the
//! thread count. It never touches the audio thread and stops between two
//! batches when its [`CancelToken`] is cancelled.

use aitrack_stock::instrument::bloom::genome::{GENES, Genome, N_GENES, SOUNDS, Species, categorical, choice_count, quantize};
use aitrack_stock::instrument::bloom::grow::{GrowOpts, generate};
use aitrack_stock::instrument::bloom::render::render_mono;
use aitrack_stock::instrument::bloom::rng::Rng;

use crate::audio::AudioData;
use crate::embedding::dsp_embedding;
use crate::progress::CancelToken;
use crate::{Error, Result};

/// Candidates render at a studio rate, like the samples they are matched
/// to (the embedding then listens at its own analysis rate).
const RENDER_RATE: u32 = 44_100;
/// Shortest and longest render a candidate gets, seconds.
const MIN_SECONDS: f32 = 0.15;
const MAX_SECONDS: f32 = 1.5;

/// How a search runs.
#[derive(Clone, Debug)]
pub struct MatchOpts {
    pub seed: u64,
    pub generations: usize,
    /// Children per generation (λ).
    pub population: usize,
    /// Survivors per generation (μ).
    pub parents: usize,
    /// Worker threads (0 = the machine's cores, at most 8).
    pub threads: usize,
    /// The sound in the cell being matched: also a starting point.
    pub start: Option<Genome>,
    /// How many distinct results to return (the best and its neighbours).
    pub results: usize,
}

impl Default for MatchOpts {
    fn default() -> Self {
        Self { seed: 1, generations: 24, population: 32, parents: 8, threads: 0, start: None, results: 4 }
    }
}

/// One generation done.
#[derive(Clone, Debug)]
pub struct MatchProgress {
    pub generation: usize,
    pub generations: usize,
    pub best: Genome,
    pub distance: f32,
}

impl MatchProgress {
    pub fn fraction(&self) -> f32 {
        if self.generations == 0 { 1.0 } else { (self.generation as f32 / self.generations as f32).clamp(0.0, 1.0) }
    }
}

/// What a search found.
#[derive(Clone, Debug)]
pub struct MatchResult {
    /// Distinct results, nearest first, with their embedding distance.
    pub found: Vec<(Genome, f32)>,
    /// The best distance after seeding and after each generation.
    pub history: Vec<f32>,
}

/// The embedding a candidate is compared by, from `seconds` of a render.
pub fn embed_genome(genome: &Genome, seconds: f32) -> Option<Vec<f32>> {
    let frames = (seconds.clamp(MIN_SECONDS, MAX_SECONDS) * RENDER_RATE as f32) as usize;
    let x = render_mono(genome, RENDER_RATE as f32, frames, 1.0);
    dsp_embedding(&AudioData::mono_signal(x, RENDER_RATE))
}

/// Euclidean distance between two embeddings.
pub fn distance(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()
}

/// How long the candidates are rendered for a target: as long as the target
/// (the embedding reads the length too), within bounds.
pub fn target_seconds(target: &AudioData) -> f32 {
    (target.duration() as f32).clamp(MIN_SECONDS, MAX_SECONDS)
}

/// The target as the candidates are compared with it: cut to the render length.
fn target_cut(target: &AudioData, seconds: f32) -> AudioData {
    let frames = (seconds * target.sample_rate as f32) as usize;
    AudioData::new(target.channels.iter().map(|c| c[..c.len().min(frames)].to_vec()).collect(), target.sample_rate)
}

struct Scorer<'a> {
    target: &'a [f32],
    seconds: f32,
    threads: usize,
    cancel: &'a CancelToken,
}

impl Scorer<'_> {
    /// Distances of `genomes` (infinite when a render had nothing to describe).
    fn score(&self, genomes: &[Genome]) -> Result<Vec<f32>> {
        self.cancel.check()?;
        let mut out = vec![f32::INFINITY; genomes.len()];
        let chunk = genomes.len().div_ceil(self.threads.max(1)).max(1);
        std::thread::scope(|s| {
            for (gs, os) in genomes.chunks(chunk).zip(out.chunks_mut(chunk)) {
                s.spawn(move || {
                    for (g, o) in gs.iter().zip(os.iter_mut()) {
                        if self.cancel.is_cancelled() {
                            return;
                        }
                        if let Some(e) = embed_genome(g, self.seconds) {
                            *o = distance(self.target, &e);
                        }
                    }
                });
            }
        });
        self.cancel.check()?;
        Ok(out)
    }
}

/// Uniform crossover of two parents (categorical genes from either).
fn crossover(a: &Genome, b: &Genome, r: &mut Rng) -> Genome {
    let mut c = *a;
    for k in 0..N_GENES {
        if r.chance(0.5) {
            c.v[k] = b.v[k];
        }
    }
    if r.chance(0.5) {
        c.role = b.role;
    }
    c
}

fn mutate(g: &Genome, sigma: f32, r: &mut Rng) -> Genome {
    let mut c = g.folded();
    for (k, (v, info)) in c.v.iter_mut().zip(GENES.iter()).enumerate() {
        let step = r.normal() * sigma;
        let flip = r.chance(0.08);
        let pick = r.unit();
        if categorical(k) {
            let n = choice_count(k);
            if flip && n > 1 && k != aitrack_stock::instrument::bloom::genome::g::CHOKE {
                let i = ((pick * n as f32) as usize).min(n - 1);
                *v = quantize(i as f32 / (n - 1) as f32);
            }
        } else if info.max > info.min {
            // Mirror at the ends, like a growth path.
            *v = quantize(aitrack_stock::instrument::bloom::genome::reflect(*v + step));
        }
    }
    c
}

/// Search for genomes that sound like `target`. `progress` hears after the
/// seeding and after every generation.
pub fn match_sample(target: &AudioData, opts: &MatchOpts, progress: &mut dyn FnMut(&MatchProgress), cancel: &CancelToken) -> Result<MatchResult> {
    let seconds = target_seconds(target);
    let embedding = dsp_embedding(&target_cut(target, seconds)).ok_or_else(|| Error::InvalidInput("the sample is silent or too short to match".into()))?;
    let threads = if opts.threads == 0 { std::thread::available_parallelism().map_or(2, |n| n.get()).min(8) } else { opts.threads };
    let scorer = Scorer { target: &embedding, seconds, threads, cancel };
    let mut r = Rng::new(opts.seed ^ 0x6E0_9A7C);

    // Seeding: one scene per species, plus the starting sound.
    let mut pool: Vec<Genome> = Vec::with_capacity(Species::ALL.len() * SOUNDS + 1);
    for (k, species) in Species::ALL.iter().enumerate() {
        let s = generate(opts.seed.wrapping_mul(31).wrapping_add(k as u64 + 1), GrowOpts { species: *species, spread: 0.6 });
        pool.extend(s.sounds.iter().copied());
    }
    if let Some(g) = opts.start {
        pool.push(g.folded());
    }
    let scores = scorer.score(&pool)?;
    let mut ranked: Vec<(Genome, f32)> = pool.into_iter().zip(scores).filter(|(_, d)| d.is_finite()).collect();
    ranked.sort_by(|a, b| a.1.total_cmp(&b.1));
    if ranked.is_empty() {
        return Err(Error::InvalidInput("no candidate could be scored".into()));
    }
    let mu = opts.parents.clamp(1, 64);
    let lambda = opts.population.max(1);
    // Keep a few more than μ for variety in the early generations.
    ranked.truncate(mu * 2);
    let mut history = vec![ranked[0].1];
    progress(&MatchProgress { generation: 0, generations: opts.generations, best: ranked[0].0, distance: ranked[0].1 });

    for step in 0..opts.generations {
        let t = step as f32 / opts.generations.max(1) as f32;
        let sigma = 0.18 * (1.0 - t) + 0.025;
        let parents = ranked.len().min(mu * if step == 0 { 2 } else { 1 });
        let mut children = Vec::with_capacity(lambda);
        for _ in 0..lambda {
            // Rank-biased choice: the best are picked most often.
            let pick = |r: &mut Rng| ((r.unit() * r.unit()) * parents as f32) as usize;
            let a = ranked[pick(&mut r).min(parents - 1)].0;
            let base = if r.chance(0.3) {
                let b = ranked[pick(&mut r).min(parents - 1)].0;
                crossover(&a, &b, &mut r)
            } else {
                a
            };
            children.push(mutate(&base, sigma, &mut r));
        }
        let scores = scorer.score(&children)?;
        ranked.extend(children.into_iter().zip(scores).filter(|(_, d)| d.is_finite()));
        ranked.sort_by(|a, b| a.1.total_cmp(&b.1));
        ranked.truncate(mu * 2);
        history.push(ranked[0].1);
        progress(&MatchProgress { generation: step + 1, generations: opts.generations, best: ranked[0].0, distance: ranked[0].1 });
    }

    // Distinct results: the best, then the nearest that differ from it.
    let mut found: Vec<(Genome, f32)> = Vec::new();
    for (g, d) in &ranked {
        if found.len() >= opts.results.max(1) {
            break;
        }
        if found.iter().all(|(f, _)| f.gene_distance(g) > 0.15) {
            found.push((*g, *d));
        }
    }
    Ok(MatchResult { found, history })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target_of(genome: &Genome, seconds: f32) -> AudioData {
        let frames = (seconds * 48_000.0) as usize;
        AudioData::mono_signal(render_mono(genome, 48_000.0, frames, 1.0), 48_000)
    }

    #[test]
    fn the_search_gets_closer_to_a_sound_bloom_made() {
        // A snare and a hat from a scene the seeding never sees.
        for (seed, cell) in [(9_001u64, 2usize), (9_002, 6)] {
            let scene = generate(seed, GrowOpts { species: Species::Kit, spread: 1.0 });
            let target = target_of(&scene.sounds[cell], 0.5);
            let opts = MatchOpts { seed: 3, generations: 10, population: 20, parents: 6, threads: 4, start: None, results: 3 };
            let mut seen = 0;
            let r = match_sample(&target, &opts, &mut |_| seen += 1, &CancelToken::new()).unwrap();
            assert_eq!(seen, 11, "seeding plus one report per generation");
            let (first, last) = (r.history[0], *r.history.last().unwrap());
            assert!(r.history.windows(2).all(|w| w[1] <= w[0]), "elitist: never worse");
            assert!(last < first * 0.85, "cell {cell}: {first} -> {last}");
            assert!(last < 2.0, "cell {cell}: ends near the target ({last})");
            assert!(!r.found.is_empty() && r.found[0].1 == last);
            // What it reports is what the found sound measures.
            let again = distance(&dsp_embedding(&target_cut(&target, target_seconds(&target))).unwrap(), &embed_genome(&r.found[0].0, target_seconds(&target)).unwrap());
            assert!((again - last).abs() < 1e-4);
            println!("cell {cell}: {:?}", r.history);
        }
    }

    #[test]
    fn a_search_is_reproducible_and_cancellable() {
        let scene = generate(77, GrowOpts::default());
        let target = target_of(&scene.sounds[0], 0.4);
        let opts = MatchOpts { seed: 5, generations: 2, population: 8, parents: 4, threads: 3, start: Some(scene.sounds[0]), results: 2 };
        let a = match_sample(&target, &opts, &mut |_| {}, &CancelToken::new()).unwrap();
        let b = match_sample(&target, &MatchOpts { threads: 1, ..opts.clone() }, &mut |_| {}, &CancelToken::new()).unwrap();
        assert_eq!(a.history, b.history, "the thread count does not change the result");
        // Starting from the target itself finds it at once.
        assert!(a.history[0] < 0.3, "{}", a.history[0]);
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(match_sample(&target, &opts, &mut |_| {}, &cancel), Err(Error::Cancelled)));
        let silent = AudioData::mono_signal(vec![0.0; 4_800], 48_000);
        assert!(match_sample(&silent, &opts, &mut |_| {}, &CancelToken::new()).is_err());
    }

    /// Timbre change grows with distance from the core, measured by the
    /// embedding the matcher uses.
    #[test]
    fn sounds_drift_further_from_their_core_with_distance() {
        let ds = [0.0f32, 0.2, 0.5, 1.0];
        let mut sums = [0f32; 4];
        let mut monotone = 0;
        let mut cells = 0;
        for seed in [1u64, 2] {
            let s = generate(seed, GrowOpts::default());
            for i in 0..SOUNDS {
                let core = embed_genome(&s.cores[i].folded(), 0.6).unwrap();
                let dist: Vec<f32> = ds
                    .iter()
                    .map(|d| {
                        let g = aitrack_stock::instrument::bloom::grow::grow(&s.cores[i], s.paths[i], *d);
                        embed_genome(&g, 0.6).map_or(0.0, |e| distance(&core, &e))
                    })
                    .collect();
                assert!(dist[0] < 1e-4, "distance 0 is the core itself");
                for k in 0..4 {
                    sums[k] += dist[k];
                }
                if dist[1] <= dist[2] && dist[2] <= dist[3] {
                    monotone += 1;
                }
                cells += 1;
            }
        }
        assert!(sums[1] < sums[2] && sums[2] < sums[3], "{sums:?}");
        assert!(monotone * 10 >= cells * 6, "{monotone} of {cells} cells drift steadily");
    }
}
