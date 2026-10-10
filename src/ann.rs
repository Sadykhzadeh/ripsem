use instant_distance::{Builder, HnswMap, Point, Search};
use serde::{Deserialize, Serialize};

pub const MIN_RECORDS: usize = 20_000;

#[derive(Clone, Serialize, Deserialize)]
pub struct Embedding(pub Vec<f32>);

impl Point for Embedding {
    fn distance(&self, other: &Self) -> f32 {
        1.0 - crate::quant::dot_f32(&self.0, &other.0)
    }
}

pub fn build(vectors: Vec<Vec<f32>>) -> Option<Vec<u8>> {
    if vectors.is_empty() {
        return None;
    }
    let values: Vec<u32> = (0..vectors.len() as u32).collect();
    let points: Vec<Embedding> = vectors.into_iter().map(Embedding).collect();
    let map = Builder::default().seed(1).build(points, values);
    bincode::serde::encode_to_vec(&map, bincode::config::legacy()).ok()
}

pub fn load(blob: &[u8]) -> Option<HnswMap<Embedding, u32>> {
    bincode::serde::decode_from_slice(blob, bincode::config::legacy())
        .ok()
        .map(|(map, _)| map)
}

pub fn search_map(map: &HnswMap<Embedding, u32>, query: &[f32], k: usize) -> Vec<(usize, f32)> {
    let mut search = Search::default();
    let point = Embedding(query.to_vec());
    map.search(&point, &mut search)
        .take(k)
        .map(|item| (*item.value as usize, 1.0 - item.distance))
        .collect()
}

pub fn search(blob: &[u8], query: &[f32], k: usize) -> Option<Vec<(usize, f32)>> {
    let map = load(blob)?;
    Some(search_map(&map, query, k))
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

    #[test]
    fn load_and_search_map_match_search() {
        let vectors: Vec<Vec<f32>> = (0..40).map(|i| unit(40, i)).collect();
        let blob = build(vectors).unwrap();

        let map = load(&blob).unwrap();
        let hits = search_map(&map, &unit(40, 7), 3);
        assert_eq!(hits[0].0, 7);
        assert!(hits[0].1 > 0.9);

        // search() wrapper must produce the same top result
        let hits2 = search(&blob, &unit(40, 7), 3).unwrap();
        assert_eq!(hits[0].0, hits2[0].0);
    }
}
