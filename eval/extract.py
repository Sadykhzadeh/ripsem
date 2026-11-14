"""Finds documented functions in a source tree and cuts their doc comments out.

Used by bench.py to build query set 1: the first sentence of a function's doc
comment is the query, the function is the answer, and the comment is removed
from the copy of the tree that gets indexed.

Rust, Go and TypeScript are read line by line, on the layout their formatters
produce (a function ends at the first closing brace indented like its first
line). Python goes through the standard library's ast.
"""

import ast
import re

SKIP_DIRS = {
    "test", "tests", "__tests__", "testdata", "testing", "integration",
    "examples", "example", "sample", "samples", "docs", "doc", "benches",
    "benchmarks", "vendor", "node_modules", "fixtures", "e2e", "caddytest",
    ".github", "scripts", "tools",
}

STOPWORDS = frozenset(
    """a an and are as at be been but by can could did do does for from had has
    have how if in into is it its of on or so than that the their them then
    there these they this those to was were what when where which while who
    whom whose why will with would you your not no any all each one also may
    should only more most such other about over under per via""".split()
)


def is_target_file(path):
    parts = path.lower().split("/")
    name = parts[-1]
    if any(p in SKIP_DIRS for p in parts[:-1]):
        return False
    return not (
        name.endswith("_test.go")
        or ".spec." in name
        or ".test." in name
        or name.startswith("test_")
        or name == "conftest.py"
        or name.endswith(".d.ts")
    )


def split_name(name):
    name = re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", name)
    name = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1 \2", name)
    return [t.lower() for t in re.split(r"[_\W]+", name) if t]


def content_words(text):
    return [w for w in re.findall(r"[a-z0-9]+", text.lower()) if w not in STOPWORDS]


def first_sentence(doc):
    """First sentence of the first paragraph, as plain prose, or None."""
    paragraph = []
    for line in doc.split("\n"):
        if not line.strip():
            if paragraph:
                break
            continue
        if line.strip().startswith(("```", "#", "|", "- ", "* ", ">")) and not paragraph:
            return None
        paragraph.append(line.strip())
    text = " ".join(paragraph)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"\{@\w+\s+([^}|]*)[^}]*\}", r"\1", text)
    text = re.sub(r"\[\[([^\]]*)\]\]", r"\1", text)
    text = re.sub(r"\[([^\]\n]+)\](?!\()", r"\1", text)
    text = text.replace("`", "").replace("**", "")
    text = re.sub(r":(?:func|class|meth|attr|mod|exc|ref|data):", "", text)
    text = re.sub(r"\s+", " ", text).strip()
    match = re.search(r"[.!?](?=\s+[A-Z0-9\"'(\[]|\s*$)", text)
    sentence = text[: match.start()] if match else text
    return sentence.strip() or None


def usable_query(sentence, name):
    """The filters of query set 1: at least five words, not the name spelled out."""
    if not sentence:
        return False
    words = re.findall(r"[A-Za-z0-9']+", sentence)
    if len(words) < 5 or len(words) > 40:
        return False
    low = sentence.lower()
    if any(m in low for m in ("todo", "fixme", "deprecated", "see also", "http://", "https://")):
        return False
    wanted = content_words(sentence)
    own = set(split_name(name))
    if wanted and sum(w in own for w in wanted) / len(wanted) >= 0.8:
        return False
    return True


def leaks_name(sentence, name):
    """Whether the query contains the answer's own name, whole or word by word."""
    words = set(re.findall(r"[a-z0-9]+", sentence.lower()))
    parts = split_name(name)
    return name.lower() in words or (len(parts) >= 2 and all(p in words for p in parts))


# --- Python --------------------------------------------------------------

def python_candidates(text):
    try:
        tree = ast.parse(text)
    except (SyntaxError, ValueError, RecursionError):
        return []
    lines = text.split("\n")
    found = []
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        first = node.body[0]
        if not (
            len(node.body) > 1
            and isinstance(first, ast.Expr)
            and isinstance(first.value, ast.Constant)
            and isinstance(first.value.value, str)
            and first.lineno > node.lineno
            and first.end_lineno is not None
        ):
            continue
        # Only a docstring that owns its lines can be cut out cleanly.
        a, b = first.lineno, first.end_lineno
        opener = lines[a - 1].lstrip().lower().lstrip("rbu")
        if not opener.startswith(('"""', "'''", '"', "'")):
            continue
        tail = lines[b - 1].rstrip()
        if not tail.endswith(('"""', "'''", '"', "'")):
            continue
        doc = ast.get_docstring(node, clean=True) or ""
        found.append(
            dict(name=node.name, start=node.lineno, end=node.end_lineno, doc=doc, cut=(a, b))
        )
    return found


# --- line-oriented languages --------------------------------------------

def indent_of(line):
    return len(line) - len(line.lstrip(" \t"))


def block_end(lines, i, limit=400):
    """1-based line where the function starting at index i ends, or None."""
    head = lines[i].rstrip()
    if head.endswith(";") and "{" not in head:
        return None
    if "{" in head and re.search(r"\}[;,)]*$", head) and head.count("{") == head.count("}"):
        return i + 1
    pad = lines[i][: indent_of(lines[i])]
    for j in range(i + 1, min(len(lines), i + limit)):
        if lines[j].startswith(pad + "}"):
            return j + 1
    return None


RUST_FN = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?"
    r"(?:extern\s+\"[^\"]*\"\s+)?fn\s+(\w+)"
)
RUST_ATTR = re.compile(r"^\s*#\[.*\]\s*$")


def rust_candidates(text):
    lines = text.split("\n")
    found = []
    for i, line in enumerate(lines):
        m = RUST_FN.match(line)
        if not m:
            continue
        j = i - 1
        attrs = []
        while j >= 0 and RUST_ATTR.match(lines[j]):
            attrs.append(lines[j])
            j -= 1
        if any("test" in a or "cfg" in a for a in attrs):
            continue
        end_doc = j
        while j >= 0 and re.match(r"^\s*///(?!/)", lines[j]):
            j -= 1
        if j == end_doc:
            continue
        doc_lines = [re.sub(r"^\s*///\s?", "", l) for l in lines[j + 1 : end_doc + 1]]
        end = block_end(lines, i)
        if end is None:
            continue
        found.append(
            dict(name=m.group(1), start=i + 1, end=end, doc="\n".join(doc_lines),
                 cut=(j + 2, end_doc + 1))
        )
    return found


GO_FN = re.compile(r"^func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)")


def go_candidates(text):
    lines = text.split("\n")
    found = []
    for i, line in enumerate(lines):
        m = GO_FN.match(line)
        if not m:
            continue
        j = i - 1
        while j >= 0 and lines[j].startswith("//") and not re.match(r"//(go:|\s*\+build|nolint)", lines[j]):
            j -= 1
        if j == i - 1:
            continue
        doc = "\n".join(re.sub(r"^//\s?", "", l) for l in lines[j + 1 : i])
        end = block_end(lines, i)
        if end is None:
            continue
        found.append(dict(name=m.group(1), start=i + 1, end=end, doc=doc, cut=(j + 2, i)))
    return found


TS_FUNCTION = re.compile(r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*(\w+)")
TS_METHOD = re.compile(
    r"^\s*(?:(?:public|private|protected|static|async|abstract|override|readonly|get|set)\s+)*"
    r"(\w+)\s*(?:<[^>(]*>)?\s*\("
)
TS_ARROW = re.compile(
    r"^\s*(?:export\s+)?(?:const|let)\s+(\w+)\s*(?::[^=]*)?=\s*(?:async\s*)?(?:\(|\w+\s*=>|<)"
)
TS_NOT_METHODS = {"if", "for", "while", "switch", "catch", "return", "function", "constructor", "super", "await", "new"}


def ts_candidates(text):
    lines = text.split("\n")
    found = []
    for i, line in enumerate(lines):
        name = None
        for pattern in (TS_FUNCTION, TS_ARROW, TS_METHOD):
            m = pattern.match(line)
            if m:
                name = m.group(1)
                break
        if not name or name in TS_NOT_METHODS:
            continue
        j = i - 1
        while j >= 0 and re.match(r"^\s*@\w+", lines[j]):
            j -= 1
        if j < 0 or not lines[j].rstrip().endswith("*/"):
            continue
        k = j
        while k >= 0 and "/**" not in lines[k]:
            k -= 1
            if j - k > 80:
                k = -1
                break
        if k < 0 or lines[k].lstrip().startswith("/***"):
            continue
        raw = lines[k : j + 1]
        doc_lines = []
        for l in raw:
            l = re.sub(r"^\s*/\*\*+\s?", "", l)
            l = re.sub(r"\s*\*+/\s*$", "", l)
            l = re.sub(r"^\s*\*\s?", "", l)
            if l.lstrip().startswith("@"):
                break
            doc_lines.append(l)
        end = block_end(lines, i)
        if end is None:
            continue
        found.append(
            dict(name=name, start=i + 1, end=end, doc="\n".join(doc_lines), cut=(k + 1, j + 1))
        )
    return found


EXTRACTORS = {
    "python": python_candidates,
    "rust": rust_candidates,
    "go": go_candidates,
    "typescript": ts_candidates,
}


def candidates(language, path, text):
    """Documented functions of one file that make a usable query."""
    out = []
    for c in EXTRACTORS[language](text):
        sentence = first_sentence(c["doc"])
        if usable_query(sentence, c["name"]):
            c["path"] = path
            c["query"] = sentence
            c["leaks_name"] = leaks_name(sentence, c["name"])
            out.append(c)
    return out


def cut_lines(text, ranges):
    """The text without the 1-based inclusive line ranges, and a mapper from
    old line numbers to new ones."""
    lines = text.split("\n")
    drop = set()
    for a, b in ranges:
        drop.update(range(a, b + 1))
    kept = [l for n, l in enumerate(lines, 1) if n not in drop]
    ordered = sorted(drop)

    def new_number(old):
        import bisect

        return old - bisect.bisect_left(ordered, old)

    return "\n".join(kept), new_number
