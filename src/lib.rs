use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use tree_sitter::{Node, Parser};

pub const MAX_FILES: usize = 64;
pub const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const PARALLEL_MIN_BYTES: usize = 64 * 1024;

/// Physical cores where supported; num_cpus falls back to logical CPUs otherwise.
pub fn default_threads() -> usize {
    num_cpus::get_physical().max(1)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Unit {
    pub name: String,
    pub line: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostic {
    #[serde(flatten)]
    pub unit: Unit,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Extraction {
    pub units: Vec<Unit>,
    pub parse_has_error: bool,
    pub omitted: Vec<Diagnostic>,
    pub recovered: Vec<Diagnostic>,
}

pub fn cpp_parser() -> Result<Parser, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .map_err(|e| e.to_string())?;
    Ok(parser)
}

// Cursor traversal is iterative: deeply nested input does not recurse in Rust.
fn visit(root: Node<'_>, mut f: impl FnMut(Node<'_>)) {
    let mut cursor = root.walk();
    loop {
        f(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

fn name_of(mut node: Node<'_>, source: &str) -> String {
    loop {
        if let Some(child) = node.child_by_field_name("declarator") {
            node = child;
            continue;
        }
        if node.kind().ends_with("declarator") {
            let mut cursor = node.walk();
            let next = node
                .named_children(&mut cursor)
                .find(|c| c.kind().ends_with("declarator"));
            if let Some(child) = next {
                node = child;
                continue;
            }
        }
        return source[node.byte_range()]
            .rsplit("::")
            .next()
            .unwrap_or("anonymous")
            .to_owned();
    }
}

fn recoverable_signature(node: Node<'_>, body: Node<'_>) -> bool {
    if body.has_error() {
        return false;
    }
    let mut seen = false;
    let mut valid = true;
    visit(node, |issue| {
        if issue.is_error() || issue.is_missing() {
            seen = true;
            valid &= issue.is_missing()
                && issue.kind() == "type_identifier"
                && issue
                    .parent()
                    .is_some_and(|p| p.kind() == "compound_literal_expression")
                && issue.end_byte() <= body.start_byte();
        }
    });
    seen && valid
}

pub fn extract(parser: &mut Parser, source: &str) -> Result<Extraction, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err("source exceeds 4 MiB".into());
    }
    let tree = parser.parse(source, None).ok_or("parse cancelled")?;
    let mut output = Extraction {
        units: vec![],
        parse_has_error: tree.root_node().has_error(),
        omitted: vec![],
        recovered: vec![],
    };
    visit(tree.root_node(), |node| {
        if node.kind() != "function_definition" {
            return;
        }
        let body = node.child_by_field_name("body");
        let recovered = node.has_error() && body.is_some_and(|b| recoverable_signature(node, b));
        let accepted = body.is_some() && (!node.has_error() || recovered);
        let start = if accepted {
            node.parent()
                .filter(|p| p.kind() == "template_declaration")
                .unwrap_or(node)
        } else {
            node
        };
        let unit = Unit {
            name: node
                .child_by_field_name("declarator")
                .map(|n| name_of(n, source))
                .unwrap_or_else(|| "anonymous".into()),
            line: start.start_position().row + 1,
            end: node.end_position().row + 1,
        };
        if accepted {
            output.units.push(unit.clone());
            if recovered {
                output.recovered.push(Diagnostic {
                    unit,
                    reason: "default_brace_signature_missing_type".into(),
                });
            }
        } else {
            output.omitted.push(Diagnostic {
                unit,
                reason: if node.has_error() {
                    "parse_error"
                } else {
                    "no_body"
                }
                .into(),
            });
        }
    });
    Ok(output)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputFile {
    pub path: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: u64,
    pub files: Vec<InputFile>,
}

#[derive(Debug, Serialize)]
pub struct FileResult {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extraction: Option<Extraction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub id: Option<u64>,
    pub results: Vec<FileResult>,
    pub parallel: bool,
    pub worker_threads: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn error(id: Option<u64>, message: impl Into<String>) -> Self {
        Self {
            id,
            results: vec![],
            parallel: false,
            worker_threads: 0,
            error: Some(message.into()),
        }
    }
}

thread_local! { static PARSER: RefCell<Result<Parser, String>> = RefCell::new(cpp_parser()); }

pub struct Extractor {
    parser: Parser,
    threads: usize,
    pool: Option<rayon::ThreadPool>,
}

impl Extractor {
    pub fn new(threads: usize) -> Result<Self, String> {
        if threads == 0 {
            return Err("threads must be a positive integer".into());
        }
        Ok(Self {
            parser: cpp_parser()?,
            threads,
            pool: None,
        })
    }

    pub fn process(&mut self, request: Request) -> Response {
        if request.files.len() > MAX_FILES {
            self.pool = None;
            return Response::error(Some(request.id), "batch exceeds 64 files");
        }
        let bytes: usize = request.files.iter().map(|f| f.source.len()).sum();
        if bytes > MAX_REQUEST_BYTES {
            self.pool = None;
            return Response::error(Some(request.id), "batch source exceeds 16 MiB");
        }
        let capped_threads = self.threads.min(request.files.len());
        let parallel = capped_threads > 1 && bytes >= PARALLEL_MIN_BYTES;
        let worker_threads = if parallel {
            capped_threads
        } else {
            request.files.len().min(1)
        };
        if self
            .pool
            .as_ref()
            .is_some_and(|pool| !parallel || pool.current_num_threads() != worker_threads)
        {
            // Reuse a matching pool, but never retain an oversized pool after a smaller batch.
            self.pool = None;
        }
        if parallel && self.pool.is_none() {
            match rayon::ThreadPoolBuilder::new()
                .num_threads(worker_threads)
                .build()
            {
                Ok(pool) => {
                    self.pool = Some(pool);
                }
                Err(e) => return Response::error(Some(request.id), e.to_string()),
            }
        }
        fn result(file: &InputFile, parser: &mut Parser) -> FileResult {
            let parsed = if file.path.len() > 4096 {
                Err("path label exceeds 4096 bytes".into())
            } else {
                extract(parser, &file.source)
            };
            match parsed {
                Ok(extraction) => FileResult {
                    path: file.path.clone(),
                    extraction: Some(extraction),
                    error: None,
                },
                Err(error) => FileResult {
                    path: file.path.clone(),
                    extraction: None,
                    error: Some(error),
                },
            }
        }
        let results = if parallel {
            self.pool.as_ref().unwrap().install(|| {
                request
                    .files
                    .par_iter()
                    .map(|file| {
                        PARSER.with(|cell| match cell.borrow_mut().as_mut() {
                            Ok(parser) => result(file, parser),
                            Err(error) => FileResult {
                                path: file.path.clone(),
                                extraction: None,
                                error: Some(error.clone()),
                            },
                        })
                    })
                    .collect()
            })
        } else {
            request
                .files
                .iter()
                .map(|file| result(file, &mut self.parser))
                .collect()
        };
        Response {
            id: Some(request.id),
            results,
            parallel,
            worker_threads: self
                .pool
                .as_ref()
                .map_or(worker_threads, |pool| pool.current_num_threads()),
            error: None,
        }
    }
}
