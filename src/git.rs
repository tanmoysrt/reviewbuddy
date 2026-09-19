//! Thin wrapper around the `git` CLI.
//!
//! Shelling out keeps us honest about rename detection, ignore rules and
//! worktree state without dragging in libgit2.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;

/// Where the content of one side of the comparison comes from.
#[derive(Clone, Debug)]
pub enum Side {
    /// A resolved commit.
    Rev(String),
    /// The files as they currently sit on disk.
    Worktree,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
}

impl Status {
    fn from_code(code: &str) -> Status {
        match code.as_bytes().first() {
            Some(b'A') => Status::Added,
            Some(b'D') => Status::Deleted,
            Some(b'R') => Status::Renamed,
            Some(b'C') => Status::Copied,
            Some(b'T') => Status::TypeChanged,
            _ => Status::Modified,
        }
    }

    /// True when the base side has no content for this entry.
    pub fn is_new(self) -> bool {
        matches!(self, Status::Added | Status::Untracked)
    }

    pub fn is_deleted(self) -> bool {
        self == Status::Deleted
    }
}

/// One changed file in the comparison.
#[derive(Clone, Debug, Serialize)]
pub struct FileEntry {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: Status,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
}

impl FileEntry {
    /// The path this file had on the base side.
    pub fn base_path(&self) -> &str {
        self.old_path.as_deref().unwrap_or(&self.path)
    }
}

/// A commit, described for display.
#[derive(Clone, Debug, Serialize)]
pub struct RefInfo {
    pub label: String,
    pub sha: String,
    pub short: String,
    pub subject: String,
}

pub struct Repo {
    pub root: PathBuf,
}

impl Repo {
    /// Walks up from `start` looking for the enclosing work tree.
    pub fn discover(start: &Path) -> Result<Repo> {
        let out = capture(start, &["rev-parse", "--show-toplevel"])
            .map_err(|_| anyhow!("not inside a git repository: {}", start.display()))?;
        let root = PathBuf::from(String::from_utf8_lossy(&out).trim().to_string());
        if root.as_os_str().is_empty() {
            bail!("not inside a git repository: {}", start.display());
        }
        Ok(Repo { root })
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        capture(&self.root, args)
    }

    fn text(&self, args: &[&str]) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.run(args)?).trim_end().to_string())
    }

    /// Runs git, returning `None` instead of an error when it exits non-zero.
    fn try_text(&self, args: &[&str]) -> Option<String> {
        self.text(args).ok().filter(|s| !s.is_empty())
    }

    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    /// Branch name, or `None` when HEAD is detached.
    pub fn current_branch(&self) -> Option<String> {
        self.try_text(&["symbolic-ref", "--quiet", "--short", "HEAD"])
    }

    /// Resolves a user supplied ref, falling back to the `origin/` remote copy.
    pub fn resolve(&self, rev: &str) -> Result<String> {
        let remote = format!("origin/{rev}");
        for candidate in [rev, remote.as_str()] {
            let spec = format!("{candidate}^{{commit}}");
            if let Some(sha) = self.try_text(&["rev-parse", "--verify", "--quiet", &spec]) {
                return Ok(sha);
            }
        }
        bail!("cannot resolve '{rev}' to a commit (tried '{rev}' and 'origin/{rev}')")
    }

    /// Best guess at the branch a review should be based on.
    pub fn default_base(&self) -> String {
        if let Some(head) = self.try_text(&["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
        {
            if let Some(name) = head.strip_prefix("origin/") {
                return name.to_string();
            }
        }
        for candidate in ["main", "master", "develop"] {
            if self.resolve(candidate).is_ok() {
                return candidate.to_string();
            }
        }
        "main".to_string()
    }

    pub fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        self.try_text(&["merge-base", a, b])
            .ok_or_else(|| anyhow!("'{a}' and '{b}' have no common ancestor"))
    }

    /// Looks up the one-line description of a commit.
    pub fn describe(&self, label: impl Into<String>, sha: &str) -> RefInfo {
        let raw = self
            .try_text(&["log", "-1", "--format=%h%x1f%s", sha])
            .unwrap_or_default();
        let (short, subject) = raw.split_once('\u{1f}').unwrap_or((&raw, ""));
        RefInfo {
            label: label.into(),
            sha: sha.to_string(),
            short: short.to_string(),
            subject: subject.to_string(),
        }
    }

    /// Every file that differs between `base` and `head`.
    pub fn changed_files(&self, base: &str, head: &Side) -> Result<Vec<FileEntry>> {
        let mut stats = self.numstat(base, head)?;
        let mut entries = Vec::new();

        let mut args = vec!["diff", "--name-status", "--find-renames", "-z", base];
        if let Side::Rev(rev) = head {
            args.push(rev);
        }
        let fields = nul_fields(&self.run(&args)?);
        let mut cursor = fields.iter();
        while let Some(code) = cursor.next() {
            let status = Status::from_code(code);
            let (old_path, path) = match status {
                Status::Renamed | Status::Copied => {
                    let old = cursor.next().cloned().unwrap_or_default();
                    let new = cursor.next().cloned().unwrap_or_default();
                    (Some(old), new)
                }
                _ => (None, cursor.next().cloned().unwrap_or_default()),
            };
            let (additions, deletions, binary) = stats.remove(&path).unwrap_or((0, 0, false));
            entries.push(FileEntry { path, old_path, status, additions, deletions, binary });
        }

        if matches!(head, Side::Worktree) {
            entries.extend(self.untracked_entries()?);
        }

        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    /// Per-file line counts, keyed by the file's path on the head side.
    fn numstat(&self, base: &str, head: &Side) -> Result<std::collections::HashMap<String, (u32, u32, bool)>> {
        let mut args = vec!["diff", "--numstat", "--find-renames", "-z", base];
        if let Side::Rev(rev) = head {
            args.push(rev);
        }
        let fields = nul_fields(&self.run(&args)?);
        let mut stats = std::collections::HashMap::new();
        let mut cursor = fields.iter();
        while let Some(record) = cursor.next() {
            // `<added>\t<deleted>\t<path>`, or a trailing empty path for renames,
            // where the old and new paths follow as their own records.
            let mut parts = record.splitn(3, '\t');
            let added = parts.next().unwrap_or("-");
            let deleted = parts.next().unwrap_or("-");
            let path = match parts.next().unwrap_or("") {
                "" => {
                    cursor.next();
                    cursor.next().cloned().unwrap_or_default()
                }
                inline => inline.to_string(),
            };
            let binary = added == "-";
            stats.insert(
                path,
                (added.parse().unwrap_or(0), deleted.parse().unwrap_or(0), binary),
            );
        }
        Ok(stats)
    }

    /// Untracked-but-not-ignored files, presented as additions.
    fn untracked_entries(&self) -> Result<Vec<FileEntry>> {
        let fields = nul_fields(&self.run(&["ls-files", "--others", "--exclude-standard", "-z"])?);
        Ok(fields
            .into_iter()
            .map(|path| {
                let content = self.read_worktree(&path).ok().flatten();
                let (additions, binary) = match content {
                    Some(bytes) if !is_binary(&bytes) => {
                        (String::from_utf8_lossy(&bytes).lines().count() as u32, false)
                    }
                    Some(_) => (0, true),
                    None => (0, false),
                };
                FileEntry {
                    path,
                    old_path: None,
                    status: Status::Untracked,
                    additions,
                    deletions: 0,
                    binary,
                }
            })
            .collect())
    }

    /// Reads a file from either side. `None` means the path is absent there.
    pub fn read(&self, side: &Side, path: &str) -> Result<Option<Vec<u8>>> {
        match side {
            Side::Rev(rev) => Ok(self.run(&["show", &format!("{rev}:{path}")]).ok()),
            Side::Worktree => self.read_worktree(path),
        }
    }

    fn read_worktree(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let full = self.safe_join(path)?;
        match std::fs::read(&full) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", full.display())),
        }
    }

    /// Resolves a repo-relative path, refusing anything that escapes the root.
    fn safe_join(&self, path: &str) -> Result<PathBuf> {
        let candidate = Path::new(path);
        if candidate.is_absolute()
            || candidate.components().any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!("path escapes the repository: {path}");
        }
        Ok(self.root.join(candidate))
    }

    /// Every tracked file on the given side, for the file browser.
    pub fn list_files(&self, side: &Side) -> Result<Vec<String>> {
        let mut files = match side {
            Side::Rev(rev) => {
                let out = self.run(&["ls-tree", "-r", "--name-only", "-z", rev])?;
                nul_fields(&out)
            }
            Side::Worktree => {
                let out = self.run(&["ls-files", "--cached", "--others", "--exclude-standard", "-z"])?;
                nul_fields(&out)
            }
        };
        files.sort();
        files.dedup();
        Ok(files)
    }
}

/// Runs git in `dir`, returning stdout or the failure's stderr.
fn capture(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(dir)
        .output()
        .context("failed to run git; is it installed and on PATH?")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

/// Splits `-z` output into its NUL separated records.
fn nul_fields(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).to_string())
        .collect()
}

/// Mirrors git's own heuristic: a NUL byte near the start means binary.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}
