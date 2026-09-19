//! The local HTTP server and its JSON API.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use percent_encoding::percent_decode_str;
use serde::Serialize;
use serde_json::json;
use tiny_http::{Header, Request, Response, Server};

use crate::diff::{self, Context, Input};
use crate::git::{self, FileEntry, RefInfo, Repo, Side};

/// What to diff against what. The configured branch comparison by default, or
/// a single commit against its parent when the UI asks for one.
#[derive(Clone)]
pub struct Comparison {
    pub base: String,
    pub head: Side,
}

impl Comparison {
    fn key(&self) -> String {
        match &self.head {
            Side::Rev(rev) => format!("{}..{}", self.base, rev),
            Side::Worktree => format!("{}..worktree", self.base),
        }
    }
}

const INDEX_HTML: &str = include_str!("web/index.html");
const APP_JS: &str = include_str!("web/app.js");
const STYLE_CSS: &str = include_str!("web/style.css");

/// JetBrains Mono, bundled so the UI looks the same on a machine that does not
/// have it installed and without reaching out to a font CDN. SIL OFL 1.1; the
/// license travels with the files in `src/web/fonts/`.
const FONT_REGULAR: &[u8] = include_bytes!("web/fonts/JetBrainsMono-Regular.woff2");
const FONT_ITALIC: &[u8] = include_bytes!("web/fonts/JetBrainsMono-Italic.woff2");
const FONT_BOLD: &[u8] = include_bytes!("web/fonts/JetBrainsMono-Bold.woff2");

pub struct App {
    pub repo: Repo,
    /// The commit every file is compared against.
    pub base: String,
    pub head: Side,
    pub base_info: RefInfo,
    pub head_info: RefInfo,
    pub merge_base: bool,
    pub context: usize,
    /// Changed-file lists per comparison, refreshed whenever the UI asks for
    /// metadata; commit ranges never change, so their entries stay valid.
    files: Mutex<HashMap<String, Vec<FileEntry>>>,
}

impl App {
    pub fn new(
        repo: Repo,
        base: String,
        head: Side,
        base_info: RefInfo,
        head_info: RefInfo,
        merge_base: bool,
        context: usize,
    ) -> App {
        App {
            repo,
            base,
            head,
            base_info,
            head_info,
            merge_base,
            context,
            files: Mutex::new(HashMap::new()),
        }
    }

    /// The branch comparison reviewbuddy was started with.
    pub fn branch_comparison(&self) -> Comparison {
        Comparison { base: self.base.clone(), head: self.head.clone() }
    }

    /// Reads the `commit` parameter, falling back to the branch comparison.
    fn comparison(&self, params: &HashMap<String, String>) -> Result<Comparison> {
        let Some(rev) = params.get("commit").filter(|v| !v.is_empty()) else {
            return Ok(self.branch_comparison());
        };
        let sha = self.repo.resolve(rev)?;
        Ok(Comparison { base: self.repo.first_parent(&sha), head: Side::Rev(sha) })
    }

    /// The changed files for a comparison. `refresh` re-reads from git, which
    /// matters only for the working tree since it changes under us.
    pub fn files(&self, comparison: &Comparison, refresh: bool) -> Result<Vec<FileEntry>> {
        let key = comparison.key();
        if !refresh {
            if let Some(hit) = self.files.lock().unwrap().get(&key) {
                return Ok(hit.clone());
            }
        }
        let entries = self.repo.changed_files(&comparison.base, &comparison.head)?;
        self.files.lock().unwrap().insert(key, entries.clone());
        Ok(entries)
    }

    fn entry(&self, comparison: &Comparison, path: &str) -> Result<FileEntry> {
        self.files(comparison, false)?
            .into_iter()
            .find(|e| e.path == path)
            .ok_or_else(|| anyhow!("'{path}' is not part of this comparison"))
    }
}

/// Serves until the process is interrupted.
pub fn serve(app: Arc<App>, server: Server) {
    let server = Arc::new(server);
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let (app, server) = (Arc::clone(&app), Arc::clone(&server));
            std::thread::spawn(move || {
                while let Ok(request) = server.recv() {
                    answer(&app, request);
                }
            })
        })
        .collect();
    for worker in workers {
        let _ = worker.join();
    }
}

struct Reply {
    status: u16,
    content_type: &'static str,
    cache: &'static str,
    body: Vec<u8>,
}

impl Reply {
    fn asset(content_type: &'static str, body: &str) -> Reply {
        Reply { status: 200, content_type, cache: "no-store", body: body.as_bytes().to_vec() }
    }

    /// Fonts never change within a build, so let the browser keep them.
    fn font(body: &'static [u8]) -> Reply {
        Reply {
            status: 200,
            content_type: "font/woff2",
            cache: "public, max-age=604800, immutable",
            body: body.to_vec(),
        }
    }

    fn json(value: &impl Serialize) -> Reply {
        match serde_json::to_vec(value) {
            Ok(body) => Reply { status: 200, content_type: "application/json", cache: "no-store", body },
            Err(e) => Reply::error(500, &e.to_string()),
        }
    }

    fn error(status: u16, message: &str) -> Reply {
        Reply {
            status,
            content_type: "application/json",
            cache: "no-store",
            body: serde_json::to_vec(&json!({ "error": message })).unwrap_or_default(),
        }
    }
}

fn answer(app: &App, request: Request) {
    let reply = route(app, request.url());
    let headers = [
        Header::from_bytes("Content-Type", reply.content_type).unwrap(),
        Header::from_bytes("Cache-Control", reply.cache).unwrap(),
    ];
    let response = Response::from_data(reply.body)
        .with_status_code(reply.status)
        .with_header(headers[0].clone())
        .with_header(headers[1].clone());
    let _ = request.respond(response);
}

fn route(app: &App, url: &str) -> Reply {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let params = parse_query(query);

    match path {
        "/" | "/index.html" => Reply::asset("text/html; charset=utf-8", INDEX_HTML),
        "/app.js" => Reply::asset("text/javascript; charset=utf-8", APP_JS),
        "/style.css" => Reply::asset("text/css; charset=utf-8", STYLE_CSS),
        "/fonts/JetBrainsMono-Regular.woff2" => Reply::font(FONT_REGULAR),
        "/fonts/JetBrainsMono-Italic.woff2" => Reply::font(FONT_ITALIC),
        "/fonts/JetBrainsMono-Bold.woff2" => Reply::font(FONT_BOLD),
        "/api/meta" => into_reply(meta(app, &params)),
        "/api/commits" => into_reply(commits(app)),
        "/api/diff" => into_reply(file_diff(app, &params)),
        "/api/file" => into_reply(file_view(app, &params)),
        "/api/tree" => into_reply(tree(app, &params)),
        _ => Reply::error(404, "no such endpoint"),
    }
}

fn into_reply(result: Result<Reply>) -> Reply {
    result.unwrap_or_else(|e| Reply::error(400, &e.to_string()))
}

#[derive(Serialize)]
struct Meta<'a> {
    repo: String,
    branch: Option<String>,
    base: &'a RefInfo,
    head: &'a RefInfo,
    merge_base: bool,
    worktree: bool,
    context: usize,
    files: Vec<FileEntry>,
}

fn meta(app: &App, params: &HashMap<String, String>) -> Result<Reply> {
    let comparison = app.comparison(params)?;
    let files = app.files(&comparison, true)?;
    let scoped = params.contains_key("commit");

    // When scoped to one commit, describe that commit rather than the branches.
    let (base_info, head_info);
    let (base, head) = match &comparison.head {
        Side::Rev(sha) if scoped => {
            base_info = app.repo.describe("parent", &comparison.base);
            head_info = app.repo.describe(&app.repo.describe("", sha).short, sha);
            (&base_info, &head_info)
        }
        _ => (&app.base_info, &app.head_info),
    };

    Ok(Reply::json(&Meta {
        repo: app.repo.name(),
        branch: app.repo.current_branch(),
        base,
        head,
        merge_base: app.merge_base && !scoped,
        worktree: matches!(comparison.head, Side::Worktree),
        context: app.context,
        files,
    }))
}

fn commits(app: &App) -> Result<Reply> {
    let commits = app.repo.commits(&app.base, &app.head)?;
    Ok(Reply::json(&json!({ "commits": commits })))
}

fn file_diff(app: &App, params: &HashMap<String, String>) -> Result<Reply> {
    let path = require(params, "path")?;
    let context = Context::parse(params.get("ctx").map(String::as_str).unwrap_or("3"));
    let comparison = app.comparison(params)?;
    let entry = app.entry(&comparison, path)?;

    let base_side = Side::Rev(comparison.base.clone());
    let old_raw = match entry.status.is_new() {
        true => None,
        false => app.repo.read(&base_side, entry.base_path())?,
    };
    let new_raw = match entry.status.is_deleted() {
        true => None,
        false => app.repo.read(&comparison.head, &entry.path)?,
    };

    let (old_text, old_binary) = decode(old_raw);
    let (new_text, new_binary) = decode(new_raw);
    let file = diff::build(
        Input {
            path: &entry.path,
            old_path: entry.old_path.as_deref(),
            status: entry.status,
            additions: entry.additions,
            deletions: entry.deletions,
            old_text: old_text.as_deref(),
            new_text: new_text.as_deref(),
            binary: entry.binary || old_binary || new_binary,
        },
        context,
    );
    Ok(Reply::json(&file))
}

fn file_view(app: &App, params: &HashMap<String, String>) -> Result<Reply> {
    let path = require(params, "path")?;
    let comparison = app.comparison(params)?;
    let side = match params.get("side").map(String::as_str) {
        Some("base") => Side::Rev(comparison.base),
        _ => comparison.head,
    };
    let (text, binary) = decode(app.repo.read(&side, path)?);
    Ok(Reply::json(&diff::view(path, text.as_deref(), binary)))
}

fn tree(app: &App, params: &HashMap<String, String>) -> Result<Reply> {
    let comparison = app.comparison(params)?;
    Ok(Reply::json(&json!({ "files": app.repo.list_files(&comparison.head)? })))
}

/// Decodes a blob, reporting anything that is not valid UTF-8 text as binary.
fn decode(raw: Option<Vec<u8>>) -> (Option<String>, bool) {
    match raw {
        None => (None, false),
        Some(bytes) if git::is_binary(&bytes) => (None, true),
        Some(bytes) => match String::from_utf8(bytes) {
            Ok(text) => (Some(text), false),
            Err(_) => (None, true),
        },
    }
}

fn require<'a>(params: &'a HashMap<String, String>, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("missing '{key}' parameter"))
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            let value = percent_decode_str(value).decode_utf8().ok()?;
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}
