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
            id: "potion-code-16M-v2",
            dims: 256,
            disk_mb: 32,
            multilingual: false,
            note: "Static code model: indexes in seconds, no ONNX runtime.",
        },
    ]
}

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    catalog().iter().find(|m| m.id == id)
}

/// The Hugging Face repository of a static model. A static model embeds by
/// looking each token up in a table and averaging, without the ONNX runtime.
pub fn static_repo(id: &str) -> Option<&'static str> {
    match id {
        "potion-code-16M-v2" => Some("minishlab/potion-code-16M-v2"),
        _ => None,
    }
}
