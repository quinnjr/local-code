//! The `serve_artifacts` built-in tool: starts (or reuses) the artifact HTTP
//! server and hands the model its base URL so it can showcase artifacts —
//! HTML/CSS/JS mockups, images, reports — in the user's browser.

use std::path::Path;

use daimon::tool::{Tool, ToolOutput};

use crate::artifacts::server;

/// Cap on files listed in the tool result — the listing exists so the model
/// can hand the user accurate links, not to fill the transcript.
const MAX_LISTED_FILES: usize = 50;

/// Stateless unit struct like the other built-ins: all server state lives in
/// `server`'s process-wide registry, so the fresh instance each agent rebuild
/// constructs still finds the already-running server.
pub struct ServeArtifacts;

impl Tool for ServeArtifacts {
    fn name(&self) -> &str {
        "serve_artifacts"
    }

    fn description(&self) -> &str {
        "Starts (or reuses) a localhost HTTP server that serves files from this project's \
         .local-code/artifacts/ directory, and returns its base URL. Write files there with \
         write_file — HTML/CSS/JS mockups, images, reports — then share the URL with the user \
         to showcase your work or get feedback on visual designs. The base URL serves \
         `index.html` if one exists, otherwise an index of everything served. Everything in \
         that directory is public to any process on this machine and to any page opened from \
         it — never write secrets, credentials, or content copied from outside the project \
         there. Percent-encode each path segment when sharing URLs (e.g. `my%20design.html`). \
         Localhost-only on a random port; stops when local-code exits."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    async fn execute(&self, _input: &serde_json::Value) -> daimon::Result<ToolOutput> {
        // The project root is the process CWD (established in main.rs) — the
        // same frame of reference read_file/write_file/bash already use for
        // relative paths, so `write_file .local-code/artifacts/x.html` and
        // this tool always agree on where artifacts live.
        match std::env::current_dir() {
            Ok(project_root) => run(&project_root).await,
            Err(e) => Ok(ToolOutput::error(format!(
                "could not determine the current directory: {e}"
            ))),
        }
    }
}

/// Everything `execute` does, with the project root taken explicitly so tests
/// don't touch the process-wide current directory.
async fn run(project_root: &Path) -> daimon::Result<ToolOutput> {
    let paths = match crate::config::paths::Paths::resolve(project_root) {
        Ok(paths) => paths,
        Err(e) => {
            return Ok(ToolOutput::error(format!(
                "could not resolve project paths: {e}"
            )));
        }
    };
    let dir = paths.project_config_dir.join("artifacts");
    let handle = match server::ensure_server(&dir).await {
        Ok(handle) => handle,
        Err(e) => {
            return Ok(ToolOutput::error(format!(
                "failed to start the artifact server: {e}"
            )));
        }
    };
    let base = format!("http://{}/{}", handle.addr, handle.token);
    let listing = served_listing(&dir, &base).await;
    Ok(ToolOutput::text(format!(
        "Artifact server running at {base} — files in {dir} are served there until local-code \
         exits.\n\nTo share work: write_file a file under {dir} (e.g. {dir}/mockup.html), then \
         give the user {base}/mockup.html. {base}/ serves index.html if one exists, otherwise \
         an index of everything served. Percent-encode each path segment in URLs you share \
         (e.g. my%20design.html). Never write secrets, credentials, or content copied from \
         outside the project there — everything in the directory is public to any process on \
         this machine.\n\n{listing}",
        dir = dir.display(),
    )))
}

/// A recursive `relative/path -> URL` listing of what's already served, so
/// the model can share accurate links for pre-existing artifacts without a
/// separate `glob` call.
async fn served_listing(dir: &Path, base: &str) -> String {
    // The walk is synchronous filesystem work; run it on the blocking pool
    // so a large tree doesn't stall the single-threaded tokio runtime
    // (which would freeze rendering and every other pane's stream for the
    // duration of the walk) — same pattern as the `grep` built-in.
    let dir = dir.to_path_buf();
    let walked = tokio::task::spawn_blocking(move || collect_served_urls(&dir)).await;
    let (urls, unreadable) = match walked {
        Ok(outcome) => outcome,
        Err(e) => return format!("(could not list served files: {e})"),
    };
    if urls.is_empty() && unreadable == 0 {
        return "Nothing is served yet — the directory is empty.".to_string();
    }
    let mut lines: Vec<String> = urls
        .iter()
        .take(MAX_LISTED_FILES)
        .map(|url| format!("  {url} -> {base}/{url}"))
        .collect();
    if urls.len() > MAX_LISTED_FILES {
        lines.push(format!(
            "  ... ({} more not listed)",
            urls.len() - MAX_LISTED_FILES
        ));
    }
    if unreadable > 0 {
        lines.push(format!("  ... ({unreadable} entries could not be read)"));
    }
    format!("Currently served:\n{}", lines.join("\n"))
}

/// The synchronous half of [`served_listing`]: walks `dir` and returns the
/// sorted, percent-encoded relative URLs of every servable file plus a count
/// of entries walkdir couldn't read. Dotfiles and non-UTF-8 names are
/// skipped — never served (see `server::resolve_requested_path`), so never
/// listed.
fn collect_served_urls(dir: &Path) -> (Vec<String>, usize) {
    let mut urls = Vec::new();
    let mut unreadable = 0;
    for entry in walkdir::WalkDir::new(dir) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(dir) else {
            continue;
        };
        let segments: Option<Vec<&str>> = relative
            .components()
            .map(|c| match c {
                std::path::Component::Normal(segment) => segment.to_str(),
                _ => None,
            })
            .collect();
        let Some(segments) = segments else {
            continue; // non-UTF-8 name — never served, so never listed
        };
        if segments.iter().any(|segment| segment.starts_with('.')) {
            continue; // dotfiles are never served, so never listed
        }
        let url = segments
            .iter()
            .map(|segment| crate::url::encode_path_segment(segment))
            .collect::<Vec<_>>()
            .join("/");
        urls.push(url);
    }
    urls.sort();
    (urls, unreadable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Pulls the `http://127.0.0.1:<port>/<token>` substring out of a tool
    /// result, trimming any trailing punctuation the surrounding prose added.
    fn base_url_in(output: &ToolOutput) -> &str {
        output
            .content
            .split_whitespace()
            .find(|word| word.starts_with("http://127.0.0.1:"))
            .map(|word| {
                word.trim_end_matches(|c: char| !(c.is_alphanumeric() || c == ':' || c == '/'))
            })
            .expect("tool result should contain the base URL")
    }

    #[tokio::test]
    async fn run_starts_a_server_and_reports_its_url_and_dir() {
        let dir = tempdir().unwrap();
        let output = run(dir.path()).await.unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(
            output
                .content
                .contains("Artifact server running at http://127.0.0.1:")
        );
        let expected_dir = dir.path().join(".local-code").join("artifacts");
        assert!(expected_dir.is_dir());
        assert!(output.content.contains(&expected_dir.display().to_string()));
        assert!(output.content.contains("Nothing is served yet"));
    }

    #[tokio::test]
    async fn run_reports_an_error_when_the_server_cannot_start() {
        let dir = tempdir().unwrap();
        // A FILE at .local-code makes create_dir_all(.local-code/artifacts)
        // fail with ENOTDIR — the tool must surface that as an error result,
        // not a panic or a hang.
        std::fs::write(dir.path().join(".local-code"), "not a directory").unwrap();
        let output = run(dir.path()).await.unwrap();
        assert!(output.is_error, "{}", output.content);
        assert!(
            output
                .content
                .contains("failed to start the artifact server"),
            "{}",
            output.content
        );
    }

    #[tokio::test]
    async fn a_second_run_reuses_the_same_server() {
        let dir = tempdir().unwrap();
        let first = run(dir.path()).await.unwrap();
        let second = run(dir.path()).await.unwrap();
        assert_eq!(base_url_in(&first), base_url_in(&second));
    }

    #[tokio::test]
    async fn the_reported_url_actually_serves_what_write_file_would_write() {
        let dir = tempdir().unwrap();
        let output = run(dir.path()).await.unwrap();
        // Mimic the exact flow the tool description tells the model to use.
        let artifacts = dir.path().join(".local-code").join("artifacts");
        std::fs::write(artifacts.join("mockup.html"), "<h1>v1</h1>").unwrap();

        let response = reqwest::get(format!("{}/mockup.html", base_url_in(&output)))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.text().await.unwrap(), "<h1>v1</h1>");
    }

    #[tokio::test]
    async fn pre_existing_files_are_listed_with_urls() {
        let dir = tempdir().unwrap();
        let artifacts = dir.path().join(".local-code").join("artifacts");
        std::fs::create_dir_all(artifacts.join("nested")).unwrap();
        std::fs::write(artifacts.join("nested").join("my report.md"), "# hi").unwrap();
        std::fs::write(artifacts.join(".env"), "SECRET=hunter2").unwrap();

        let output = run(dir.path()).await.unwrap();
        assert!(
            output.content.contains("Currently served:"),
            "{}",
            output.content
        );
        assert!(
            output
                .content
                .contains("nested/my%20report.md -> http://127.0.0.1:"),
            "{}",
            output.content
        );
        // Dotfiles are never served, so never listed.
        assert!(!output.content.contains(".env"), "{}", output.content);
        assert!(!output.content.contains("hunter2"), "{}", output.content);
    }

    #[tokio::test]
    async fn listing_is_truncated_at_50_files() {
        let dir = tempdir().unwrap();
        let artifacts = dir.path().join(".local-code").join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        for i in 0..51 {
            std::fs::write(artifacts.join(format!("file-{i:02}.txt")), "x").unwrap();
        }

        let output = run(dir.path()).await.unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(
            output.content.contains("(1 more not listed)"),
            "{}",
            output.content
        );
        assert_eq!(
            output.content.matches(" -> http://").count(),
            50,
            "{}",
            output.content
        );
    }

    #[tokio::test]
    async fn execute_runs_against_the_process_cwd() {
        // The one code path tests can't redirect: `execute` itself reads the
        // process CWD. cargo test runs with the crate root as CWD, and
        // `.local-code/` is gitignored there — same as a real invocation.
        let tool = ServeArtifacts;
        let output = tool.execute(&serde_json::json!({})).await.unwrap();
        assert!(
            !output.is_error,
            "cwd={:?} content={}",
            std::env::current_dir(),
            output.content
        );
        assert!(output.content.contains("http://127.0.0.1:"));
    }
}
