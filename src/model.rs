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
            note: "Default. Strong retrieval quality at a small size.",
        },
        ModelInfo {
            id: "all-MiniLM-L6-v2",
            dims: 384,
            disk_mb: 90,
            multilingual: false,
            note: "Fastest to embed, lighter on disk, lower recall.",
        },
        ModelInfo {
            id: "bge-base-en-v1.5",
            dims: 768,
            disk_mb: 436,
            multilingual: false,
            note: "Higher quality, larger and slower.",
        },
        ModelInfo {
            id: "multilingual-e5-small",
            dims: 384,
            disk_mb: 470,
            multilingual: true,
            note: "Multilingual queries and comments.",
        },
        ModelInfo {
            id: "nomic-embed-text-v1.5",
            dims: 768,
            disk_mb: 550,
            multilingual: false,
            note: "Long context, high quality.",
        },
    ]
}

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    catalog().iter().find(|m| m.id == id)
}
