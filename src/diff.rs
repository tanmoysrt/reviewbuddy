//! Turns two blobs into the rows the browser renders.

use serde::Serialize;
use similar::{Algorithm, ChangeTag, DiffOp, InlineChangeMode, InlineChangeOptions, TextDiff};

use crate::git::Status;
use crate::highlight::{self, Token};

/// How much unchanged code to keep around each change.
#[derive(Clone, Copy, Debug)]
pub enum Context {
    Lines(usize),
    Full,
}

impl Context {
    /// Parses the `ctx` query parameter: a line count, or `full`.
    pub fn parse(raw: &str) -> Context {
        match raw {
            "full" => Context::Full,
            other => Context::Lines(other.parse().unwrap_or(3).min(200)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Rendered normally.
    Ok,
    /// Not text, so there is nothing useful to show.
    Binary,
    /// Text, but the two sides are identical.
    Identical,
    /// The path does not exist on the side that was asked for.
    Missing,
}

#[derive(Debug, Serialize)]
pub struct FileDiff {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub additions: u32,
    pub deletions: u32,
    pub state: State,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Serialize)]
pub struct Hunk {
    pub header: String,
    pub rows: Vec<Row>,
}

#[derive(Debug, Serialize)]
pub struct Row {
    /// `ctx`, `del` or `add`.
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new: Option<u32>,
    pub html: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_newline: bool,
}

/// A whole file, highlighted, for the browse pane.
#[derive(Debug, Serialize)]
pub struct FileView {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub state: State,
    pub lines: Vec<String>,
}

/// Word level refinement reads better than the character level default.
fn inline_options() -> InlineChangeOptions {
    let mut options = InlineChangeOptions::new();
    options.mode(InlineChangeMode::Words).semantic_cleanup(true);
    options
}

pub struct Input<'a> {
    pub path: &'a str,
    pub old_path: Option<&'a str>,
    pub status: Status,
    pub additions: u32,
    pub deletions: u32,
    /// `None` when the file does not exist on that side.
    pub old_text: Option<&'a str>,
    pub new_text: Option<&'a str>,
    pub binary: bool,
}

pub fn build(input: Input<'_>, context: Context) -> FileDiff {
    let highlighter = highlight::shared();
    let base_path = input.old_path.unwrap_or(input.path);
    let mut file = FileDiff {
        path: input.path.to_string(),
        old_path: input.old_path.map(str::to_string),
        status: input.status,
        language: highlighter.language(input.path).or_else(|| highlighter.language(base_path)),
        additions: input.additions,
        deletions: input.deletions,
        state: State::Ok,
        hunks: Vec::new(),
    };

    if input.binary {
        file.state = State::Binary;
        return file;
    }
    let (old_text, new_text) = match (input.old_text, input.new_text) {
        (None, None) => {
            file.state = State::Missing;
            return file;
        }
        (old, new) => (old.unwrap_or(""), new.unwrap_or("")),
    };
    if old_text == new_text {
        file.state = State::Identical;
        return file;
    }

    let old_tokens = highlighter.tokenize(base_path, old_text);
    let new_tokens = highlighter.tokenize(input.path, new_text);

    let diff = TextDiff::configure()
        .algorithm(Algorithm::Histogram)
        .diff_lines(old_text, new_text);
    let groups = match context {
        Context::Full => vec![diff.ops().to_vec()],
        Context::Lines(radius) => diff.grouped_ops(radius),
    };

    let options = inline_options();
    for group in groups {
        let Some(header) = hunk_header(&group) else { continue };
        let mut rows = Vec::new();
        for op in &group {
            for change in diff.iter_inline_changes_with_options(op, options) {
                let (kind, tokens) = match change.tag() {
                    ChangeTag::Equal => ("ctx", line_tokens(&new_tokens, change.new_index())),
                    ChangeTag::Delete => ("del", line_tokens(&old_tokens, change.old_index())),
                    ChangeTag::Insert => ("add", line_tokens(&new_tokens, change.new_index())),
                };

                let (text, emphasis) = flatten(change.values());
                let trimmed = text.trim_end_matches('\n').trim_end_matches('\r');
                rows.push(Row {
                    kind,
                    old: change.old_index().map(|i| i as u32 + 1),
                    new: change.new_index().map(|i| i as u32 + 1),
                    html: highlight::render_line(trimmed, tokens, &emphasis),
                    no_newline: change.missing_newline(),
                });
            }
        }
        file.hunks.push(Hunk { header, rows });
    }
    file
}

/// Renders a whole file for the browse pane.
pub fn view(path: &str, text: Option<&str>, binary: bool) -> FileView {
    let highlighter = highlight::shared();
    let mut view = FileView {
        path: path.to_string(),
        language: highlighter.language(path),
        state: State::Ok,
        lines: Vec::new(),
    };
    let Some(text) = text else {
        view.state = State::Missing;
        return view;
    };
    if binary {
        view.state = State::Binary;
        return view;
    }

    let tokens = highlighter.tokenize(path, text);
    view.lines = text
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            let trimmed = line.trim_end_matches('\r');
            highlight::render_line(trimmed, line_tokens(&tokens, Some(index)), &[])
        })
        .collect();
    // `split` leaves a phantom empty line after a trailing newline.
    if text.ends_with('\n') {
        view.lines.pop();
    }
    view
}

/// `@@ -old,count +new,count @@`, matching what reviewers expect to see.
fn hunk_header(group: &[DiffOp]) -> Option<String> {
    let first = group.first()?;
    let last = group.last()?;
    let (old_start, new_start) = (first.old_range().start, first.new_range().start);
    let (old_end, new_end) = (last.old_range().end, last.new_range().end);
    Some(format!(
        "@@ -{},{} +{},{} @@",
        old_start + 1,
        old_end - old_start,
        new_start + 1,
        new_end - new_start
    ))
}

fn line_tokens(lines: &[Vec<Token>], index: Option<usize>) -> &[Token] {
    index.and_then(|i| lines.get(i)).map(Vec::as_slice).unwrap_or(&[])
}

/// Joins the inline-diff fragments back into a line, recording which byte
/// ranges of it the refinement marked as changed.
fn flatten(values: &[(bool, &str)]) -> (String, Vec<(usize, usize)>) {
    let mut text = String::new();
    let mut emphasis: Vec<(usize, usize)> = Vec::new();
    for (changed, fragment) in values {
        let start = text.len();
        text.push_str(fragment);
        if *changed {
            match emphasis.last_mut() {
                Some(last) if last.1 == start => last.1 = text.len(),
                _ => emphasis.push((start, text.len())),
            }
        }
    }
    // A run that covers the entire line carries no information, and painting
    // the whole row twice just makes it harder to read.
    if emphasis.len() == 1 && emphasis[0] == (0, text.trim_end().len()) {
        emphasis.clear();
    }
    (text, emphasis)
}
