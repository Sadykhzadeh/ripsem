#![cfg(feature = "fastembed")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ripsem::config::{Backend, Config};
use ripsem::pipeline;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ripsem-bench-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn write(&self, rel: &str, contents: &str) {
        std::fs::write(self.path.join(rel), contents).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

const CORPUS: &[(&str, &str)] = &[
    (
        "auth.rs",
        "fn sign_in(name, secret) {\n    let stored = lookup_account(name);\n    if hash(secret) != stored.password_hash { return Err(Denied); }\n    let token = issue_session(name);\n    set_cookie(token);\n}\nfn sign_out(token) { revoke_session(token); }\n",
    ),
    (
        "database.rs",
        "fn checkout_connection(pool) {\n    let conn = pool.acquire();\n    conn\n}\nfn run_transaction(pool, work) {\n    let tx = begin();\n    match work(tx) { Ok(v) => commit(tx), Err(e) => rollback(tx) }\n}\n",
    ),
    (
        "http_client.rs",
        "fn fetch(url) {\n    let socket = connect(url.host, 443);\n    send_request(socket, \"GET\", url.path);\n    let response = read_response(socket);\n    response.body\n}\n",
    ),
    (
        "cache.rs",
        "struct Lru { map, capacity }\nfn remember(cache, key, compute) {\n    if let Some(v) = cache.map.get(key) { return v; }\n    let v = compute();\n    if cache.map.len() >= cache.capacity { evict_least_recently_used(cache); }\n    cache.map.insert(key, v);\n}\n",
    ),
    (
        "settings.rs",
        "fn load_settings() {\n    let host = read_env(\"APP_HOST\").unwrap_or(\"localhost\");\n    let port = read_env(\"APP_PORT\").unwrap_or(\"8080\");\n    Settings { host, port }\n}\n",
    ),
    (
        "logging.rs",
        "enum Level { Debug, Info, Warn, Error }\nfn emit(level, message) {\n    let line = format(\"[{level}] {message}\");\n    append_to_logfile(line);\n}\n",
    ),
    (
        "geometry.rs",
        "fn rectangle_area(width, height) { width * height }\nfn circle_area(radius) { 3.14159 * radius * radius }\nfn perimeter(width, height) { 2 * (width + height) }\n",
    ),
    (
        "json.rs",
        "fn to_json(value) {\n    let mut out = String::new();\n    encode_value(value, out);\n    out\n}\nfn from_json(text) { decode_value(parse_tokens(text)) }\n",
    ),
    (
        "retry.rs",
        "fn with_backoff(action) {\n    let mut delay = 100;\n    for attempt in 0..5 {\n        match action() { Ok(v) => return Ok(v), Err(_) => { sleep(delay); delay = delay * 2; } }\n    }\n}\n",
    ),
    (
        "files.rs",
        "fn list_directory(dir) {\n    let mut names = Vec::new();\n    for entry in read_dir(dir) { names.push(entry.file_name()); }\n    names\n}\n",
    ),
];

const QUERIES: &[(&str, &str)] = &[
    ("how do users sign in to their account", "auth.rs"),
    ("where is the password verified before access", "auth.rs"),
    ("grab a pooled connection to the database", "database.rs"),
    ("commit or roll back a unit of database work", "database.rs"),
    (
        "send a web request and read the response body",
        "http_client.rs",
    ),
    ("store results so we avoid recomputing them", "cache.rs"),
    ("drop the least recently used entry when full", "cache.rs"),
    (
        "read configuration from environment variables",
        "settings.rs",
    ),
    ("compute the area of a rectangle", "geometry.rs"),
    ("convert a value into a json string", "json.rs"),
    (
        "write messages to a log file with severity levels",
        "logging.rs",
    ),
    ("wait longer after each failed attempt", "retry.rs"),
    ("list the file names inside a folder", "files.rs"),
    ("evict_least_recently_used", "cache.rs"),
    ("with_backoff", "retry.rs"),
    ("run_transaction rollback", "database.rs"),
];

fn rank_of(hits: &[pipeline::SearchHit], expected: &str) -> Option<usize> {
    hits.iter().position(|h| h.path == expected).map(|p| p + 1)
}

#[test]
#[ignore = "downloads the embedding model; run with --ignored --nocapture"]
fn retrieval_quality_meets_thresholds() {
    let dir = TempDir::new();
    for (name, body) in CORPUS {
        dir.write(name, body);
    }

    let config = Config {
        backend: Backend::Fastembed,
        root: dir.path.clone(),
        ..Default::default()
    };
    pipeline::index_repo(&config, true).unwrap();

    let mut hit_at_1 = 0;
    let mut hit_at_3 = 0;
    let mut reciprocal = 0.0;
    for (query, expected) in QUERIES {
        let hits = pipeline::search(&config, query, 5).unwrap();
        match rank_of(&hits, expected) {
            Some(1) => {
                hit_at_1 += 1;
                hit_at_3 += 1;
                reciprocal += 1.0;
            }
            Some(r) if r <= 3 => {
                hit_at_3 += 1;
                reciprocal += 1.0 / r as f64;
            }
            Some(r) => reciprocal += 1.0 / r as f64,
            None => eprintln!("  miss: {query:?} (expected {expected})"),
        }
    }

    let n = QUERIES.len() as f64;
    let recall_1 = hit_at_1 as f64 / n;
    let recall_3 = hit_at_3 as f64 / n;
    let mrr = reciprocal / n;
    eprintln!(
        "retrieval over {} queries: recall@1={:.2} recall@3={:.2} MRR={:.3}",
        QUERIES.len(),
        recall_1,
        recall_3,
        mrr
    );

    assert!(recall_3 >= 0.75, "recall@3 too low: {recall_3:.2}");
    assert!(mrr >= 0.65, "MRR too low: {mrr:.3}");
}
