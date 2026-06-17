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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(chunk_file("", 10, 2).is_empty());
        assert!(chunk_file("   \n\t\n", 10, 2).is_empty());
    }

    #[test]
    fn single_window_covers_short_file() {
        let chunks = chunk_file("a\nb\nc", 40, 8);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 3);
        assert_eq!(chunks[0].text, "a\nb\nc");
    }

    #[test]
    fn windows_overlap_by_step() {
        let body: String = (1..=10).map(|n| format!("line{n}\n")).collect();
        let chunks = chunk_file(&body, 4, 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 4);
        assert_eq!(chunks[1].start_line, 4);
        assert_eq!(chunks.last().unwrap().end_line, 10);
    }

    #[test]
    fn overlap_at_least_window_does_not_stall() {
        let body: String = (1..=6).map(|n| format!("line{n}\n")).collect();
        let chunks = chunk_file(&body, 3, 9);
        assert_eq!(chunks.last().unwrap().end_line, 6);
        assert!(chunks.len() <= body.lines().count());
    }
}
