//! reviewbuddy - review the diff between two branches in your browser.

mod diff;
mod git;
mod highlight;
mod server;

use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tiny_http::Server;

use crate::git::{Repo, Side};
use crate::server::App;

#[derive(Parser, Debug)]
#[command(
    name = "reviewbuddy",
    version,
    about = "Review the diff between two branches in your browser"
)]
struct Args {
    /// Branch, tag or commit to compare against (defaults to the repository's main branch)
    #[arg(short, long)]
    base: Option<String>,

    /// Compare this ref instead of the working tree
    #[arg(long)]
    head: Option<String>,

    /// Diff base directly against head instead of from where they diverged
    #[arg(long)]
    no_merge_base: bool,

    /// Lines of unchanged code to show around each change
    #[arg(short, long, default_value_t = 3)]
    context: usize,

    /// Port to listen on; 0 picks a free one
    #[arg(short, long, default_value_t = 0)]
    port: u16,

    /// Print the URL instead of opening a browser
    #[arg(long)]
    no_open: bool,
}

fn main() {
    if let Err(error) = run(Args::parse()) {
        eprintln!("reviewbuddy: {error:#}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<()> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let repo = Repo::discover(&cwd)?;

    let base_name = args.base.clone().unwrap_or_else(|| repo.default_base());
    let base_sha = repo.resolve(&base_name)?;

    let (head, head_sha, head_label) = match &args.head {
        Some(rev) => {
            let sha = repo.resolve(rev)?;
            (Side::Rev(sha.clone()), sha, rev.clone())
        }
        None => {
            let sha = repo.resolve("HEAD")?;
            let branch = repo.current_branch().unwrap_or_else(|| "HEAD".to_string());
            (Side::Worktree, sha, branch)
        }
    };

    // Three-dot by default: show what this branch adds, not what it is missing.
    let merge_base = !args.no_merge_base;
    let compare_from = match merge_base {
        true => repo.merge_base(&base_sha, &head_sha)?,
        false => base_sha.clone(),
    };
    if compare_from == head_sha && matches!(head, Side::Rev(_)) {
        bail!("'{base_name}' and '{head_label}' are the same commit; there is nothing to review");
    }

    let base_info = repo.describe(base_name.as_str(), &compare_from);
    let head_info = repo.describe(head_label.as_str(), &head_sha);

    let app = Arc::new(App::new(
        repo,
        compare_from,
        head,
        base_info,
        head_info,
        merge_base,
        args.context,
    ));
    let files = app.files(&app.branch_comparison(), true)?;

    let server = Server::http(("127.0.0.1", args.port))
        .map_err(|e| anyhow::anyhow!("cannot listen on port {}: {e}", args.port))?;
    let port = server
        .server_addr()
        .to_ip()
        .context("server did not bind to a TCP port")?
        .port();
    let url = format!("http://127.0.0.1:{port}");

    println!("reviewbuddy  {}", app.repo.name());
    println!("  base  {}  {}", app.base_info.label, app.base_info.short);
    let worktree = match app.head {
        Side::Worktree => "  + working tree",
        Side::Rev(_) => "",
    };
    println!("  head  {}  {}{}", app.head_info.label, app.head_info.short, worktree);
    println!(
        "  {} file{} changed",
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    );
    println!("\n  {url}\n");

    if args.no_open {
        println!("  (open it yourself; --no-open was passed)");
    } else if let Err(error) = open_browser(&url) {
        eprintln!("  could not open a browser ({error}); visit the URL above");
    }
    println!("  Ctrl-C to stop.");

    server::serve(app, server);
    Ok(())
}

fn open_browser(url: &str) -> Result<()> {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    Command::new(opener)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("failed to run {opener}"))?;
    Ok(())
}
