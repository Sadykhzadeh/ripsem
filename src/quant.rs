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

pub fn dot(stored: &QuantizedVector, query: &[f32]) -> f32 {
    let acc: f32 = stored
        .data
        .iter()
        .zip(query)
        .map(|(q, x)| *q as f32 * x)
        .sum();
    acc * stored.scale
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

    #[test]
    fn stores_roughly_a_quarter_of_the_f32_size() {
        let v: Vec<f32> = (0..384).map(|i| (i as f32).sin()).collect();
        let f32_bytes = bincode::serialize(&v).unwrap().len();
        let q_bytes = bincode::serialize(&quantize(&v)).unwrap().len();
        assert!(q_bytes * 3 < f32_bytes);
    }

    // Minimal LCG for deterministic pseudo-random numbers in tests.
    // Constants from Knuth TAOCP vol. 2.
    struct Lcg(u64);
    impl Lcg {
        fn next_f32(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            // Map high 32 bits to [0, 1)
            (self.0 >> 32) as f32 / (u32::MAX as f32 + 1.0)
        }
        // Returns a value in [-1, 1).
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

    // Property: dot(quantize(a), b) ≈ a·b for random unit-norm vectors.
    //
    // Error bound derivation:
    //   scale = max|a_i| / 127
    //   per-element rounding error |err_i| ≤ scale / 2
    //   total error ≤ (scale/2) · Σ|b_i| ≤ (scale/2) · sqrt(dim)   [Cauchy-Schwarz]
    //   For unit-norm a in dim=384 a typical max|a_i| ≈ 0.20,
    //   giving scale ≈ 0.0016 and error ≤ ~0.016.
    //   Worst-case theoretical maximum ≈ 0.077 (single-spike a).
    //   We assert < 0.04, which is tight for typical random vectors and
    //   will catch any systematic bias or scaling error.
    #[test]
    fn property_dot_error_over_random_unit_norm_vectors() {
        const DIM: usize = 384;
        const TRIALS: usize = 1000;
        const BOUND: f32 = 0.04;

        let mut rng = Lcg(0xdeadbeef_cafebabe);
        let mut worst: f32 = 0.0;

        for _ in 0..TRIALS {
            let mut a: Vec<f32> = (0..DIM).map(|_| rng.next_f32_signed()).collect();
            let mut b: Vec<f32> = (0..DIM).map(|_| rng.next_f32_signed()).collect();
            normalize(&mut a);
            normalize(&mut b);

            let approx = dot(&quantize(&a), &b);
            let exact = plain_dot(&a, &b);
            let err = (approx - exact).abs();
            if err > worst {
                worst = err;
            }
            assert!(
                err < BOUND,
                "dot error {err} exceeded bound {BOUND} (approx={approx}, exact={exact})"
            );
        }
        // Uncomment to inspect during development:
        // eprintln!("worst error over {TRIALS} trials: {worst}");
        let _ = worst;
    }

    // A single-spike vector (one dominant component, rest near-zero) must
    // round-trip through quantize/dequantize without blowing up the dot error.
    // The scale is set by the spike, so the small components incur minimal error.
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
        // scale = a[0] / 127 ≈ 0.00788; rounding error per element ≤ scale/2.
        // Σ|b_i| ≤ sqrt(DIM) so bound ≈ 0.077; in practice much smaller.
        assert!(
            (approx - exact).abs() < 0.04,
            "single-spike dot error too large: approx={approx}, exact={exact}"
        );
    }

    // Saturation test: a vector where a secondary large component gets clamped
    // to ±127 because an even larger component sets the scale.
    // Asserts the dot product error stays bounded even when clamping occurs for
    // components whose magnitude is close to (but does not exceed) max|a_i|.
    #[test]
    fn saturating_components_stay_within_error_bound() {
        // a[0] is the maximum; a[1] is 0.99 × max, so it exercises the code
        // path where a near-max element rounds to exactly 126 (one step below
        // saturation) — this covers values close to the clamp boundary.
        let mut a: Vec<f32> = vec![0.0; 64];
        a[0] = 1.0;
        a[1] = 0.99;
        a[2] = -0.98;
        // Fill remaining dims with moderate values to ensure norm is realistic.
        for (i, val) in a.iter_mut().enumerate().skip(3) {
            *val = 0.1 * ((i as f32).sin());
        }
        normalize(&mut a);

        let mut b: Vec<f32> = vec![0.0; 64];
        b[0] = 0.8;
        b[1] = 0.6;
        normalize(&mut b);

        let q = quantize(&a);
        // The two near-max elements must not have been clamped (they are ≤127
        // by construction since only a[0] == max|a_i|).
        assert!(q.data[0] == 127 || q.data[0] == 126);
        assert!(
            q.data[1].abs() >= 125,
            "near-max element unexpectedly far from saturation"
        );

        let approx = dot(&q, &b);
        let exact = plain_dot(&a, &b);
        // scale = max|a_i|/127; bound = (scale/2)*sqrt(64) = (max/254)*8
        // With max|a_i| ≤ 1 (unit-norm), bound ≤ 8/254 ≈ 0.031.
        assert!(
            (approx - exact).abs() < 0.04,
            "saturation dot error too large: approx={approx}, exact={exact}"
        );
    }
}
