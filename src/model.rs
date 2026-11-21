#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    pub id: &'static str,
    pub dims: usize,
    pub disk_mb: u32,
    pub multilingual: bool,
    pub note: &'static str,
}

pub const DEFAULT_MODEL: &str = "bge-small-en-v1.5";

pub fn catalog() -> &'static [ModelInfo] {
    &[
        ModelInfo {
            id: "bge-small-en-v1.5",
            dims: 384,
            disk_mb: 133,
            multilingual: false,
            note: "Default.",
        },
        ModelInfo {
            id: "all-MiniLM-L6-v2",
            dims: 384,
            disk_mb: 90,
            multilingual: false,
            note: "Smallest and fastest to embed.",
        },
        ModelInfo {
            id: "bge-base-en-v1.5",
            dims: 768,
            disk_mb: 436,
            multilingual: false,
            note: "Larger English model.",
        },
        ModelInfo {
            id: "multilingual-e5-small",
            dims: 384,
            disk_mb: 470,
            multilingual: true,
            note: "Queries and comments in other languages.",
        },
        ModelInfo {
            id: "nomic-embed-text-v1.5",
            dims: 768,
            disk_mb: 550,
            multilingual: false,
            note: "Larger English model, the most memory to index.",
        },
        ModelInfo {
            id: "jina-embeddings-v2-base-code",
            dims: 768,
            disk_mb: 644,
            multilingual: false,
            note: "Trained on code in 30 languages; the largest download.",
        },
    ]
}

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    catalog().iter().find(|m| m.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_readme_table_has_each_model_with_its_size_and_dimensions() {
        let readme = include_str!("../README.md");
        for info in catalog() {
            let row = format!("| {} | {} | ~{} MB |", info.id, info.dims, info.disk_mb);
            assert!(
                readme.contains(&row),
                "README model table has no row `{row}`"
            );
        }
        let default = readme
            .lines()
            .find(|line| line.ends_with("| default |"))
            .expect("README marks the default model");
        assert!(
            default.starts_with(&format!("| {DEFAULT_MODEL} |")),
            "{default}"
        );
    }
}
