use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantizedVector {
    pub scale: f32,
    pub data: Vec<i8>,
}

impl QuantizedVector {
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

pub fn quantize(vector: &[f32]) -> QuantizedVector {
    let max = vector.iter().fold(0f32, |m, x| m.max(x.abs()));
    if max == 0.0 {
        return QuantizedVector {
            scale: 0.0,
            data: vec![0; vector.len()],
        };
    }
    let scale = max / 127.0;
    let data = vector
        .iter()
        .map(|x| (x / scale).round().clamp(-127.0, 127.0) as i8)
        .collect();
    QuantizedVector { scale, data }
}

pub fn dequantize(stored: &QuantizedVector) -> Vec<f32> {
    stored
        .data
        .iter()
        .map(|&q| q as f32 * stored.scale)
        .collect()
}

/// Independent running sums. The compiler may not reorder float additions, so
/// one accumulator runs element by element; sixteen sums can sit side by side in
/// vector registers.
const LANES: usize = 16;

/// Scores one record against the query. Every query calls this for every
/// record.
pub fn dot(stored: &QuantizedVector, query: &[f32]) -> f32 {
    let len = stored.data.len().min(query.len());
    let (data, query) = (&stored.data[..len], &query[..len]);
    let mut sums = [0f32; LANES];
    let (data_chunks, data_tail) = data.as_chunks::<LANES>();
    let (query_chunks, query_tail) = query.as_chunks::<LANES>();
    for (q, x) in data_chunks.iter().zip(query_chunks) {
        for lane in 0..LANES {
            sums[lane] += q[lane] as f32 * x[lane];
        }
    }
    let tail: f32 = data_tail
        .iter()
        .zip(query_tail)
        .map(|(q, x)| *q as f32 * x)
        .sum();
    (sums.iter().sum::<f32>() + tail) * stored.scale
}

/// [`dot`] for two full-precision vectors, as the graph build compares them.
pub fn dot_f32(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let (a, b) = (&a[..len], &b[..len]);
    let mut sums = [0f32; LANES];
    let (a_chunks, a_tail) = a.as_chunks::<LANES>();
    let (b_chunks, b_tail) = b.as_chunks::<LANES>();
    for (x, y) in a_chunks.iter().zip(b_chunks) {
        for lane in 0..LANES {
            sums[lane] += x[lane] * y[lane];
        }
    }
    let tail: f32 = a_tail.iter().zip(b_tail).map(|(x, y)| x * y).sum();
    sums.iter().sum::<f32>() + tail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn approximates_the_true_dot_product() {
        let a = [0.20, -0.50, 0.10, 0.83, -0.12];
        let b = [0.11, 0.40, -0.30, 0.55, 0.66];
        let approx = dot(&quantize(&a), &b);
        assert!((approx - plain_dot(&a, &b)).abs() < 0.01);
    }

    #[test]
    fn preserves_ranking_order() {
        let query = [0.9, 0.1, 0.0];
        let near = quantize(&[1.0, 0.0, 0.0]);
        let far = quantize(&[0.0, 1.0, 0.0]);
        assert!(dot(&near, &query) > dot(&far, &query));
    }

    #[test]
    fn zero_vector_is_safe() {
        let q = quantize(&[0.0, 0.0, 0.0]);
        assert_eq!(q.scale, 0.0);
        assert_eq!(dot(&q, &[1.0, 1.0, 1.0]), 0.0);
    }

    /// Lengths that are not a multiple of the lane count go through the tail.
    #[test]
    fn split_sums_match_a_plain_sum_at_any_length() {
        for len in [0, 1, 3, 15, 16, 17, 37, 384, 768] {
            let a: Vec<f32> = (0..len)
                .map(|i| ((i * 7 % 11) as f32 - 5.0) / 8.0)
                .collect();
            let b: Vec<f32> = (0..len).map(|i| ((i * 3 % 5) as f32 - 2.0) / 4.0).collect();
            let exact = plain_dot(&a, &b);
            assert!((dot_f32(&a, &b) - exact).abs() < 1e-4, "dot_f32 at {len}");

            let q = quantize(&a);
            let dequantized = plain_dot(&dequantize(&q), &b);
            assert!((dot(&q, &b) - dequantized).abs() < 1e-4, "dot at {len}");
        }
    }

    #[test]
    fn mismatched_lengths_score_the_common_prefix() {
        let q = quantize(&[1.0, 1.0, 1.0]);
        assert!((dot(&q, &[1.0, 1.0]) - 2.0).abs() < 0.02);
        assert_eq!(dot_f32(&[1.0, 2.0, 3.0], &[1.0]), 1.0);
    }

    #[test]
    fn stores_roughly_a_quarter_of_the_f32_size() {
        let v: Vec<f32> = (0..384).map(|i| (i as f32).sin()).collect();
        let f32_bytes = bincode::serde::encode_to_vec(&v, bincode::config::legacy())
            .unwrap()
            .len();
        let q_bytes = bincode::serde::encode_to_vec(quantize(&v), bincode::config::legacy())
            .unwrap()
            .len();
        assert!(q_bytes * 3 < f32_bytes);
    }

    // Deterministic pseudo-random numbers for the tests.
    struct Lcg(u64);
    impl Lcg {
        fn next_f32(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 32) as f32 / (u32::MAX as f32 + 1.0)
        }

        fn next_f32_signed(&mut self) -> f32 {
            self.next_f32() * 2.0 - 1.0
        }
    }

    fn normalize(v: &mut [f32]) {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
    }

    // dot(quantize(a), b) stays within 0.04 of a·b for random unit vectors.
    // Rounding error per element is at most scale/2, so the total is at most
    // (scale/2) * sqrt(dim): about 0.016 for typical vectors at 384 dimensions,
    // 0.077 in the worst case.
    #[test]
    fn property_dot_error_over_random_unit_norm_vectors() {
        const DIM: usize = 384;
        const TRIALS: usize = 1000;
        const BOUND: f32 = 0.04;

        let mut rng = Lcg(0xdeadbeef_cafebabe);

        for _ in 0..TRIALS {
            let mut a: Vec<f32> = (0..DIM).map(|_| rng.next_f32_signed()).collect();
            let mut b: Vec<f32> = (0..DIM).map(|_| rng.next_f32_signed()).collect();
            normalize(&mut a);
            normalize(&mut b);

            let approx = dot(&quantize(&a), &b);
            let exact = plain_dot(&a, &b);
            let err = (approx - exact).abs();
            assert!(
                err < BOUND,
                "dot error {err} exceeded bound {BOUND} (approx={approx}, exact={exact})"
            );
        }
    }

    // One dominant component sets the scale, so the small ones lose precision.
    #[test]
    fn single_spike_vector_dot_is_accurate() {
        const DIM: usize = 384;
        let mut a = vec![0.01f32; DIM];
        a[0] = 1.0;
        normalize(&mut a);

        let mut b = vec![0.01f32; DIM];
        b[0] = 1.0;
        normalize(&mut b);

        let approx = dot(&quantize(&a), &b);
        let exact = plain_dot(&a, &b);
        assert!(
            (approx - exact).abs() < 0.04,
            "single-spike dot error too large: approx={approx}, exact={exact}"
        );
    }

    // Components just below the maximum quantize close to the clamp boundary.
    #[test]
    fn saturating_components_stay_within_error_bound() {
        let mut a: Vec<f32> = vec![0.0; 64];
        a[0] = 1.0;
        a[1] = 0.99;
        a[2] = -0.98;
        for (i, val) in a.iter_mut().enumerate().skip(3) {
            *val = 0.1 * ((i as f32).sin());
        }
        normalize(&mut a);

        let mut b: Vec<f32> = vec![0.0; 64];
        b[0] = 0.8;
        b[1] = 0.6;
        normalize(&mut b);

        let q = quantize(&a);
        assert!(q.data[0] == 127 || q.data[0] == 126);
        assert!(
            q.data[1].abs() >= 125,
            "near-max element unexpectedly far from saturation"
        );

        let approx = dot(&q, &b);
        let exact = plain_dot(&a, &b);
        assert!(
            (approx - exact).abs() < 0.04,
            "saturation dot error too large: approx={approx}, exact={exact}"
        );
    }
}
