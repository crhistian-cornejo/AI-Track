//! Pure geometry of the Vault editor drawings: where the FM algorithm
//! diagram puts each operator.

use aitrack_stock::instrument::vault::fm::{ALGORITHMS, Algorithm, OPS};

/// The operators `op` modulates (0-based).
pub fn targets(alg: &Algorithm, op: usize) -> Vec<usize> {
    (0..OPS).filter(|&i| alg.mods[i] & (1 << op) != 0).collect()
}

/// Whether `op` is heard.
pub fn is_carrier(alg: &Algorithm, op: usize) -> bool {
    alg.carriers & (1 << op) != 0
}

/// Grid cells of the six operators of `alg`: `(column, row)`, row 0 the
/// carriers at the bottom, each modulator one row above the highest of its
/// targets and in its first target's column when that cell is free.
/// Returns the cells and the number of columns.
pub fn fm_layout(alg: &Algorithm) -> ([(usize, usize); OPS], usize) {
    // Rows: modulators always have a higher index than their targets, so
    // walking from op 1 upwards sees every target first.
    let mut row = [0usize; OPS];
    for op in 0..OPS {
        row[op] = targets(alg, op).iter().map(|&t| row[t] + 1).max().unwrap_or(0);
    }
    let mut cells = [(usize::MAX, 0usize); OPS];
    let mut used: Vec<(usize, usize)> = Vec::new();
    let max_row = row.iter().copied().max().unwrap_or(0);
    for r in 0..=max_row {
        for op in (0..OPS).filter(|&o| row[o] == r) {
            let want = targets(alg, op).first().map_or(0, |&t| cells[t].0);
            let mut col = want;
            while used.contains(&(col, r)) {
                col += 1;
            }
            used.push((col, r));
            cells[op] = (col, r);
        }
    }
    let cols = cells.iter().map(|c| c.0 + 1).max().unwrap_or(1);
    (cells, cols)
}

/// The layout of algorithm `index` (clamped).
pub fn algorithm(index: usize) -> &'static Algorithm {
    &ALGORITHMS[index.min(ALGORITHMS.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stack_is_one_column_and_all_carriers_one_row() {
        let (cells, cols) = fm_layout(algorithm(0));
        assert_eq!(cols, 1);
        for (op, c) in cells.iter().enumerate() {
            assert_eq!(*c, (0, op), "op {} sits above op {op}", op + 1);
        }
        let (cells, cols) = fm_layout(algorithm(9));
        assert_eq!(cols, 6);
        assert!(cells.iter().all(|c| c.1 == 0));
    }

    #[test]
    fn every_algorithm_lays_out_without_overlaps() {
        for (i, alg) in ALGORITHMS.iter().enumerate() {
            let (cells, cols) = fm_layout(alg);
            for a in 0..OPS {
                assert!(cells[a].0 < cols);
                for b in a + 1..OPS {
                    assert_ne!(cells[a], cells[b], "algorithm {i}: ops {} and {} overlap", a + 1, b + 1);
                }
                for t in targets(alg, a) {
                    assert!(cells[a].1 > cells[t].1, "algorithm {i}: op {} is drawn above op {}", a + 1, t + 1);
                }
                if is_carrier(alg, a) && targets(alg, a).is_empty() {
                    assert_eq!(cells[a].1, 0, "carriers sit on the bottom row");
                }
            }
        }
        // Five into one: one carrier, five modulators side by side above it.
        let (cells, cols) = fm_layout(algorithm(6));
        assert_eq!((cells[0], cols), ((0, 0), 5));
        assert!(cells[1..].iter().all(|c| c.1 == 1));
    }
}
