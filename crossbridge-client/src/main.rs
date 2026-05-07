//! `crossbridge-client` CLI entry point.
//!
//! See `.design/client.md` for the full spec. The shape:
//!
//!   crossbridge-client peers
//!   crossbridge-client submit --issue <id> --target <slug>
//!   crossbridge-client answer --issue <id>

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use crossbridge_client::labels;
use crossbridge_client::peers::list_peers;
use crossbridge_client::slug::derive_own_slug;
use crossbridge_client::socket_root;
use crossbridge_protocol::{
    read_message_sync, write_message_sync, AnswerComment, ClientRequest, ServerResponse,
    SubmitAnswer, SubmitIssue,
};
use crosslink::db::Database;

#[derive(Parser)]
#[command(version, about = "Per-agent client for crossbridge")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List currently-registered peer repos by slug, one per line.
    Peers,
    /// Submit a local issue to a peer repo.
    Submit {
        #[arg(long)]
        issue: i64,
        #[arg(long)]
        target: String,
    },
    /// Answer an inbound crossbridge issue, sending its result comments back
    /// to the source repo.
    Answer {
        #[arg(long)]
        issue: i64,
    },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Peers => peers_cmd(),
        Cmd::Submit { issue, target } => submit_cmd(issue, &target),
        Cmd::Answer { issue } => answer_cmd(issue),
    }
}

// ---- peers -----------------------------------------------------------------

fn peers_cmd() -> Result<()> {
    let cwd = std::env::current_dir().context("could not read current directory")?;
    let repo_root = repo_root_from(&cwd)?;
    let own = derive_own_slug(&repo_root)?;
    let dir = socket_dir(&own);
    let peers = list_peers(&dir)?;
    for p in peers {
        println!("{p}");
    }
    Ok(())
}

// ---- submit ----------------------------------------------------------------

fn submit_cmd(issue_id: i64, target: &str) -> Result<()> {
    let cwd = std::env::current_dir().context("could not read current directory")?;
    let repo_root = repo_root_from(&cwd)?;
    let own = derive_own_slug(&repo_root)?;
    let db_path = repo_root.join(".crosslink").join("issues.db");
    let db = Database::open(&db_path)
        .with_context(|| format!("failed to open crosslink DB at {}", db_path.display()))?;

    let issue = db
        .get_issue(issue_id)?
        .ok_or_else(|| anyhow!("issue #{issue_id} not found"))?;
    let source_uuid = db
        .get_issue_uuid_by_id(issue.id)
        .with_context(|| format!("issue #{issue_id} has no UUID in the local DB"))?;
    let local_labels = db.get_labels(issue.id)?;

    // Verify the target socket exists before doing any DB writes.
    let socket_path = socket_dir(&own).join(format!("{target}.socket"));
    if !socket_path.exists() {
        bail!("peer '{target}' not available (not connected)");
    }

    let request = ClientRequest::Submit(SubmitIssue {
        title: issue.title,
        body: issue.description.unwrap_or_default(),
        labels: local_labels,
        source_slug: own,
        source_uuid: source_uuid.clone(),
        attachments: Vec::new(),
    });

    let response = round_trip(&socket_path, target, &request)?;
    let target_id = match response {
        ServerResponse::Ok { issue_id } => issue_id,
        ServerResponse::Error { message } => bail!("{message}"),
    };

    // The xb-ref value must equal `source_uuid` — that's what the answerer
    // echoes back in `SubmitAnswer.source_uuid`, and what the server uses to
    // locate the outbound issue when routing the answer. Labelling with the
    // receiver's i64 issue_id (which the wire's `Ok { issue_id }` carries)
    // would break the answer round-trip.
    db.add_label(issue.id, labels::OUTBOUND)?;
    db.add_label(issue.id, labels::STATUS_PENDING)?;
    db.add_label(issue.id, &labels::ref_label(&source_uuid))?;

    println!("submitted issue #{issue_id} to '{target}' (remote id {target_id})");
    Ok(())
}

// ---- answer ----------------------------------------------------------------

fn answer_cmd(issue_id: i64) -> Result<()> {
    let cwd = std::env::current_dir().context("could not read current directory")?;
    let repo_root = repo_root_from(&cwd)?;
    let own = derive_own_slug(&repo_root)?;
    let db_path = repo_root.join(".crosslink").join("issues.db");
    let db = Database::open(&db_path)
        .with_context(|| format!("failed to open crosslink DB at {}", db_path.display()))?;

    let issue = db
        .get_issue(issue_id)?
        .ok_or_else(|| anyhow!("issue #{issue_id} not found"))?;
    let local_labels = db.get_labels(issue.id)?;

    if !labels::has(&local_labels, labels::INBOUND) {
        bail!("issue #{issue_id} is not an inbound crossbridge issue");
    }
    let source_slug =
        labels::find_prefixed(&local_labels, labels::SOURCE_PREFIX).ok_or_else(|| {
            anyhow!(
                "issue #{issue_id} is missing required label '{}<slug>'",
                labels::SOURCE_PREFIX
            )
        })?;
    let source_uuid =
        labels::find_prefixed(&local_labels, labels::REF_PREFIX).ok_or_else(|| {
            anyhow!(
                "issue #{issue_id} is missing required label '{}<uuid>'",
                labels::REF_PREFIX
            )
        })?;

    let comments = db.get_comments(issue.id)?;
    let answer_comments: Vec<AnswerComment> = comments
        .into_iter()
        .filter(|c| c.kind == "result")
        .map(|c| AnswerComment {
            content: c.content,
            kind: c.kind,
        })
        .collect();

    let socket_path = socket_dir(&own).join(format!("{source_slug}.socket"));
    if !socket_path.exists() {
        bail!("peer '{source_slug}' not available (not connected)");
    }

    let request = ClientRequest::Answer(SubmitAnswer {
        source_uuid: source_uuid.to_string(),
        comments: answer_comments,
        attachments: Vec::new(),
    });

    let response = round_trip(&socket_path, source_slug, &request)?;
    match response {
        ServerResponse::Ok { issue_id: remote } => {
            db.add_label(issue.id, labels::STATUS_ANSWERED)?;
            db.close_issue(issue.id)?;
            println!("answered issue #{issue_id} -> '{source_slug}' (remote id {remote})");
            Ok(())
        }
        ServerResponse::Error { message } => bail!("{message}"),
    }
}

// ---- shared plumbing -------------------------------------------------------

fn socket_dir(own_slug: &str) -> PathBuf {
    socket_root().join(own_slug)
}

fn round_trip(
    socket_path: &Path,
    peer_label: &str,
    request: &ClientRequest,
) -> Result<ServerResponse> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| anyhow!("cannot reach peer '{peer_label}': {e}"))?;
    write_message_sync(&mut stream, request)
        .with_context(|| format!("failed to send request to peer '{peer_label}'"))?;
    read_message_sync(&mut stream)
        .with_context(|| format!("failed to read response from peer '{peer_label}'"))
}

/// Walk up from `start` looking for a `.crosslink/issues.db` (the project root
/// marker we actually need). Falls back to `start` itself if nothing is found,
/// letting the DB-open call surface the real error.
fn repo_root_from(start: &Path) -> Result<PathBuf> {
    let mut cur = start;
    loop {
        if cur.join(".crosslink").join("issues.db").exists() {
            return Ok(cur.to_path_buf());
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return Ok(start.to_path_buf()),
        }
    }
}
