//! Syntax highlighting.
//!
//! syntect parses with Sublime grammars, which gives us scope stacks rather
//! than colours. We collapse those stacks onto a handful of short CSS class
//! names so the theme lives in the stylesheet and both themes come for free.

use std::sync::OnceLock;

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

/// Files above these limits are served as plain text; the parser is the slow
/// part of a request and nobody reviews a 50k line file line by line.
const MAX_HIGHLIGHT_BYTES: usize = 2 * 1024 * 1024;
const MAX_HIGHLIGHT_LINES: usize = 40_000;

/// A classified byte range within a single line.
#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub class: &'static str,
}

/// Scope prefixes mapped to CSS classes, most specific first.
///
/// `punctuation` is deliberately absent: leaving it unmatched lets a string's
/// quotes fall through to the enclosing `string` scope, which is what readers
/// expect.
const RULES: &[(&str, &str)] = &[
    ("comment", "c"),
    ("string", "s"),
    ("constant.numeric", "n"),
    ("constant.language", "k"),
    ("constant.character.escape", "e"),
    ("constant", "n"),
    ("keyword.operator", "o"),
    ("keyword", "k"),
    ("storage", "k"),
    ("entity.name.function", "f"),
    ("support.function", "f"),
    ("entity.name", "t"),
    ("entity.other.inherited-class", "t"),
    ("support.type", "t"),
    ("support.class", "t"),
    ("support.constant", "n"),
    ("variable.language", "k"),
    ("variable.function", "f"),
];

/// Extensions with no grammar of their own that read well as another language.
const ALIASES: &[(&str, &str)] = &[
    ("ts", "JavaScript"),
    ("tsx", "JavaScript"),
    ("jsx", "JavaScript"),
    ("mjs", "JavaScript"),
    ("cjs", "JavaScript"),
    ("mts", "JavaScript"),
    ("cts", "JavaScript"),
    ("h", "C++"),
    ("hpp", "C++"),
    ("kt", "Java"),
    ("kts", "Java"),
    ("vue", "HTML"),
    ("svelte", "HTML"),
    ("zsh", "Bourne Again Shell (bash)"),
    ("fish", "Bourne Again Shell (bash)"),
    ("yml", "YAML"),
];

pub struct Highlighter {
    syntaxes: SyntaxSet,
    rules: Vec<(Scope, &'static str)>,
}

/// Loading the grammar set costs ~50ms and a few MB, so do it once.
pub fn shared() -> &'static Highlighter {
    static INSTANCE: OnceLock<Highlighter> = OnceLock::new();
    INSTANCE.get_or_init(|| Highlighter {
        syntaxes: SyntaxSet::load_defaults_newlines(),
        rules: RULES
            .iter()
            .filter_map(|(scope, class)| Some((Scope::new(scope).ok()?, *class)))
            .collect(),
    })
}

impl Highlighter {
    fn syntax_for(&self, path: &str) -> &SyntaxReference {
        let name = path.rsplit('/').next().unwrap_or(path);
        let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
        self.syntaxes
            .find_syntax_by_extension(name)
            .or_else(|| self.syntaxes.find_syntax_by_extension(extension))
            .or_else(|| {
                let (_, alias) = ALIASES.iter().find(|(ext, _)| *ext == extension)?;
                self.syntaxes.find_syntax_by_name(alias)
            })
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }

    /// Human readable language name, or `None` when we have no grammar for it.
    pub fn language(&self, path: &str) -> Option<String> {
        let syntax = self.syntax_for(path);
        (syntax.name != "Plain Text").then(|| syntax.name.clone())
    }

    /// Classifies every line of `text`. The result always has one entry per
    /// line, empty where nothing is highlighted.
    pub fn tokenize(&self, path: &str, text: &str) -> Vec<Vec<Token>> {
        let line_count = lines_with_endings(text).count();
        if text.len() > MAX_HIGHLIGHT_BYTES || line_count > MAX_HIGHLIGHT_LINES {
            return vec![Vec::new(); line_count];
        }

        let syntax = self.syntax_for(path);
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut lines = Vec::with_capacity(line_count);

        for line in lines_with_endings(text) {
            let Ok(ops) = state.parse_line(line, &self.syntaxes) else {
                // A grammar that trips over one line will not recover; the rest
                // of the file is still perfectly readable unhighlighted.
                lines.resize(line_count, Vec::new());
                break;
            };

            let mut tokens: Vec<Token> = Vec::new();
            let mut start = 0;
            for (index, op) in ops {
                push_token(&mut tokens, start, index, self.class_for(&stack));
                if stack.apply(&op).is_err() {
                    break;
                }
                start = index;
            }
            push_token(&mut tokens, start, line.len(), self.class_for(&stack));
            lines.push(tokens);
        }
        lines
    }

    /// The class of the innermost scope we have an opinion about.
    fn class_for(&self, stack: &ScopeStack) -> &'static str {
        for scope in stack.as_slice().iter().rev() {
            for (prefix, class) in &self.rules {
                if prefix.is_prefix_of(*scope) {
                    return class;
                }
            }
        }
        ""
    }
}

fn push_token(tokens: &mut Vec<Token>, start: usize, end: usize, class: &'static str) {
    if end <= start || class.is_empty() {
        return;
    }
    match tokens.last_mut() {
        Some(last) if last.end == start && last.class == class => last.end = end,
        _ => tokens.push(Token { start, end, class }),
    }
}

/// Splits text into lines, keeping the line terminator, as the grammars expect.
fn lines_with_endings(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let split = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
        let (line, tail) = rest.split_at(split);
        rest = tail;
        Some(line)
    })
}

/// Renders one line as HTML, overlaying inline-diff emphasis on the syntax
/// classes. Both are byte ranges over the same text, so we cut the line at
/// every boundary either one introduces.
pub fn render_line(text: &str, tokens: &[Token], emphasis: &[(usize, usize)]) -> String {
    let len = text.len();
    if len == 0 {
        return String::new();
    }

    let mut cuts: Vec<usize> = vec![0, len];
    for token in tokens {
        cuts.push(token.start.min(len));
        cuts.push(token.end.min(len));
    }
    for (start, end) in emphasis {
        cuts.push((*start).min(len));
        cuts.push((*end).min(len));
    }
    cuts.retain(|c| text.is_char_boundary(*c));
    cuts.sort_unstable();
    cuts.dedup();

    let mut html = String::with_capacity(len + 32);
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let class = tokens
            .iter()
            .find(|t| t.start <= start && start < t.end)
            .map(|t| t.class)
            .unwrap_or("");
        let emphasized = emphasis.iter().any(|(s, e)| *s <= start && start < *e);

        match (class, emphasized) {
            ("", false) => escape_into(&mut html, &text[start..end]),
            ("", true) => {
                html.push_str("<span class=\"x\">");
                escape_into(&mut html, &text[start..end]);
                html.push_str("</span>");
            }
            (class, emphasized) => {
                html.push_str("<span class=\"");
                html.push_str(class);
                if emphasized {
                    html.push_str(" x");
                }
                html.push_str("\">");
                escape_into(&mut html, &text[start..end]);
                html.push_str("</span>");
            }
        }
    }
    html
}

fn escape_into(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}
