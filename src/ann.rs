use instant_distance::{Builder, HnswMap, Point, Search};
use serde::{Deserialize, Serialize};

pub const MIN_RECORDS: usize = 20_000;

#[derive(Clone, Serialize, Deserialize)]
pub struct Embedding(pub Vec<f32>);

impl Point for Embedding {
    fn distance(&self, other: &Self) -> f32 {
        let dot: f32 = self.0.iter().zip(&other.0).map(|(a, b)| a * b).sum();
        1.0 - dot
    }
}

pub fn build(vectors: Vec<Vec<f32>>) -> Option<Vec<u8>> {
    if vectors.is_empty() {
        return None;
    }
    let values: Vec<u32> = (0..vectors.len() as u32).collect();
    let points: Vec<Embedding> = vectors.into_iter().map(Embedding).collect();
    let map = Builder::default().seed(1).build(points, values);
    bincode::serialize(&map).ok()
}

pub fn search(blob: &[u8], query: &[f32], k: usize) -> Option<Vec<(usize, f32)>> {
    let map: HnswMap<Embedding, u32> = bincode::deserialize(blob).ok()?;
    let mut search = Search::default();
    let point = Embedding(query.to_vec());
    let hits = map
        .search(&point, &mut search)
        .take(k)
        .map(|item| (*item.value as usize, 1.0 - item.distance))
        .collect();
    Some(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(dims: usize, hot: usize) -> Vec<f32> {
        let mut v = vec![0.0; dims];
        v[hot] = 1.0;
        v
    }

    #[test]
    fn finds_the_nearest_vector() {
        let vectors: Vec<Vec<f32>> = (0..40).map(|i| unit(40, i)).collect();
        let blob = build(vectors).unwrap();

        let hits = search(&blob, &unit(40, 7), 3).unwrap();
        assert_eq!(hits[0].0, 7);
        assert!(hits[0].1 > 0.9);
    }

    #[test]
    fn empty_input_builds_nothing() {
        assert!(build(Vec::new()).is_none());
    }
}
