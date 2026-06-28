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
}
