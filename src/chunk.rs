#[derive(Debug, Clone)]
pub struct Chunk {
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
}

pub fn chunk_file(text: &str, chunk_lines: usize, overlap: usize) -> Vec<Chunk> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let window = chunk_lines.max(1);
    let step = window.saturating_sub(overlap).max(1);

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let end = (start + window).min(lines.len());
        let body = lines[start..end].join("\n");
        if !body.trim().is_empty() {
            chunks.push(Chunk {
                start_line: start + 1,
                end_line: end,
                text: body,
            });
        }
        if end == lines.len() {
            break;
        }
        start += step;
    }
    chunks
}
