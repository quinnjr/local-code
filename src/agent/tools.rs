//! Built-in tools: the six `#[tool_fn]`-annotated functions below. Permission
//! enforcement does not live in these bodies — see `crate::agent::gated_tool::GatedTool`,
//! which wraps each of these before registration.

use daimon::tool::ToolOutput;
use daimon::tool_fn;

/// `read_file` refuses to read files larger than this — an unbounded read
/// would balloon the transcript (every read result is retained and
/// re-persisted to the session file on every subsequent turn) and risks a
/// large-file OOM. 2 MiB comfortably covers real source files while bounding
/// the worst case.
const MAX_READ_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Reads the full contents of a file at `path` (absolute or relative to the
/// process's current working directory). Refuses files larger than
/// [`MAX_READ_FILE_BYTES`] — use `grep`/`bash` to inspect a large file instead
/// of reading it whole.
#[tool_fn]
async fn read_file(
    /// Path to the file to read.
    path: String,
) -> daimon::Result<ToolOutput> {
    match tokio::fs::metadata(&path).await {
        Ok(meta) if meta.len() > MAX_READ_FILE_BYTES => {
            return Ok(ToolOutput::error(format!(
                "{path} is {} bytes, which exceeds the {MAX_READ_FILE_BYTES}-byte read_file limit; \
                 use grep to search it or bash (e.g. head/tail) to inspect it in parts instead",
                meta.len()
            )));
        }
        Ok(_) => {}
        Err(e) => return Ok(ToolOutput::error(format!("failed to read {path}: {e}"))),
    }
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => Ok(ToolOutput::text(content)),
        Err(e) => Ok(ToolOutput::error(format!("failed to read {path}: {e}"))),
    }
}

/// Writes `content` to `path`, creating the file (and parent directories) if it
/// doesn't exist, or overwriting it entirely if it does. For targeted changes to
/// an existing file, prefer `edit_file`.
#[tool_fn]
async fn write_file(
    /// Path to the file to write.
    path: String,
    /// The full content to write to the file.
    content: String,
) -> daimon::Result<ToolOutput> {
    let path_ref = std::path::Path::new(&path);
    if let Some(parent) = path_ref.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = tokio::fs::create_dir_all(parent).await
    {
        return Ok(ToolOutput::error(format!(
            "failed to create parent directories for {path}: {e}"
        )));
    }
    match tokio::fs::write(&path, content).await {
        Ok(()) => Ok(ToolOutput::text(format!("wrote {path}"))),
        Err(e) => Ok(ToolOutput::error(format!("failed to write {path}: {e}"))),
    }
}

/// Replaces the single occurrence of `find` with `replace` inside the file at
/// `path`. Fails (without modifying the file) if `find` occurs zero times or more
/// than once, so the caller must supply enough surrounding context to make `find`
/// unique — this is a targeted find/replace, not a whole-file overwrite.
#[tool_fn]
async fn edit_file(
    /// Path to the file to edit.
    path: String,
    /// The exact text to find. Must occur exactly once in the file.
    find: String,
    /// The text to replace it with.
    replace: String,
) -> daimon::Result<ToolOutput> {
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => return Ok(ToolOutput::error(format!("failed to read {path}: {e}"))),
    };

    let occurrences = content.matches(find.as_str()).count();
    if occurrences == 0 {
        return Ok(ToolOutput::error(format!("find text not found in {path}")));
    }
    if occurrences > 1 {
        return Ok(ToolOutput::error(format!(
            "find text is ambiguous in {path}: occurs {occurrences} times, expected exactly 1"
        )));
    }

    let updated = content.replacen(find.as_str(), &replace, 1);
    match tokio::fs::write(&path, updated).await {
        Ok(()) => Ok(ToolOutput::text(format!("edited {path}"))),
        Err(e) => Ok(ToolOutput::error(format!("failed to write {path}: {e}"))),
    }
}

/// Executes `command` as a shell command (`sh -c`) and returns combined stdout and
/// stderr along with the exit code. Subject to the active permission tier.
#[tool_fn]
async fn bash(
    /// The shell command to execute.
    command: String,
) -> daimon::Result<ToolOutput> {
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .output()
        .await;

    match output {
        Ok(out) => {
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&out.stdout));
            combined.push_str(&String::from_utf8_lossy(&out.stderr));
            let exit_code = out.status.code().unwrap_or(-1);
            let text = format!("exit code: {exit_code}\n{combined}");
            if out.status.success() {
                Ok(ToolOutput::text(text))
            } else {
                Ok(ToolOutput::error(text))
            }
        }
        Err(e) => Ok(ToolOutput::error(format!("failed to execute command: {e}"))),
    }
}

/// Searches for lines matching a regular expression `pattern` inside files under
/// `path` (defaults to the current directory), recursively. Returns up to 200
/// matches as `file:line: text`.
#[tool_fn]
async fn grep(
    /// Regular expression to search for.
    pattern: String,
    /// Directory to search under. Defaults to the current directory if omitted.
    path: Option<String>,
) -> daimon::Result<ToolOutput> {
    let root = path.unwrap_or_else(|| ".".to_string());
    let regex = match regex::Regex::new(&pattern) {
        Ok(r) => r,
        Err(e) => return Ok(ToolOutput::error(format!("invalid regex '{pattern}': {e}"))),
    };

    // The walk + reads are synchronous filesystem work; run them on the
    // blocking pool so a large tree doesn't stall the single-threaded tokio
    // runtime (which would freeze rendering and every other pane's stream
    // for the duration of the search).
    let walked = tokio::task::spawn_blocking(move || {
        let mut matches = Vec::new();
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            // Same cap `read_file` enforces: reading a multi-GB file whole
            // just to line-scan it is the OOM hazard that limit exists for.
            if entry
                .metadata()
                .map(|m| m.len() > MAX_READ_FILE_BYTES)
                .unwrap_or(false)
            {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for (line_no, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    matches.push(format!(
                        "{}:{}: {}",
                        entry.path().display(),
                        line_no + 1,
                        line
                    ));
                    if matches.len() >= 200 {
                        break;
                    }
                }
            }
            if matches.len() >= 200 {
                break;
            }
        }
        matches
    })
    .await;
    let matches = match walked {
        Ok(matches) => matches,
        Err(e) => return Ok(ToolOutput::error(format!("grep failed: {e}"))),
    };

    if matches.is_empty() {
        Ok(ToolOutput::text("no matches found"))
    } else {
        Ok(ToolOutput::text(matches.join("\n")))
    }
}

/// Finds files whose path matches a glob `pattern` (e.g. `**/*.rs`) under `path`
/// (defaults to the current directory). Returns up to 200 matches, one per line.
#[tool_fn]
async fn glob(
    /// Glob pattern to match, relative to `path` (e.g. `**/*.rs`).
    pattern: String,
    /// Directory to search under. Defaults to the current directory if omitted.
    path: Option<String>,
) -> daimon::Result<ToolOutput> {
    let root = path.unwrap_or_else(|| ".".to_string());
    let full_pattern = format!("{}/{}", root.trim_end_matches('/'), pattern);

    let paths = match glob::glob(&full_pattern) {
        Ok(p) => p,
        Err(e) => {
            return Ok(ToolOutput::error(format!(
                "invalid glob '{full_pattern}': {e}"
            )));
        }
    };

    // Iterating the glob walks the filesystem; blocking pool for the same
    // single-threaded-runtime reason as `grep` above.
    let walked = tokio::task::spawn_blocking(move || {
        let mut matches = Vec::new();
        for p in paths.flatten() {
            matches.push(p.display().to_string());
            if matches.len() >= 200 {
                break;
            }
        }
        matches
    })
    .await;
    let matches = match walked {
        Ok(matches) => matches,
        Err(e) => return Ok(ToolOutput::error(format!("glob failed: {e}"))),
    };

    if matches.is_empty() {
        Ok(ToolOutput::text("no matches found"))
    } else {
        Ok(ToolOutput::text(matches.join("\n")))
    }
}

#[cfg(test)]
mod builtin_tool_tests {
    use super::*;
    use daimon::tool::Tool;
    use tempfile::tempdir;

    #[tokio::test]
    async fn read_file_returns_contents() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("hello.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let tool = ReadFile;
        let output = tool
            .execute(&serde_json::json!({"path": file_path.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(output.content, "hello world");
    }

    #[tokio::test]
    async fn read_file_refuses_a_file_larger_than_the_cap() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("big.txt");
        std::fs::write(&file_path, vec![b'x'; (MAX_READ_FILE_BYTES + 1) as usize]).unwrap();

        let tool = ReadFile;
        let output = tool
            .execute(&serde_json::json!({"path": file_path.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("exceeds"));
    }

    #[tokio::test]
    async fn read_file_missing_file_is_an_error_output_not_a_panic() {
        let tool = ReadFile;
        let output = tool
            .execute(&serde_json::json!({"path": "/nonexistent/path/x.txt"}))
            .await
            .unwrap();
        assert!(output.is_error);
    }

    #[tokio::test]
    async fn write_file_creates_file_and_parent_dirs() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("nested").join("out.txt");

        let tool = WriteFile;
        let output = tool
            .execute(&serde_json::json!({
                "path": file_path.to_str().unwrap(),
                "content": "new content"
            }))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "new content");
    }

    #[tokio::test]
    async fn edit_file_replaces_unique_match() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("code.rs");
        std::fs::write(&file_path, "fn main() { println!(\"old\"); }").unwrap();

        let tool = EditFile;
        let output = tool
            .execute(&serde_json::json!({
                "path": file_path.to_str().unwrap(),
                "find": "old",
                "replace": "new"
            }))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "fn main() { println!(\"new\"); }"
        );
    }

    #[tokio::test]
    async fn edit_file_rejects_ambiguous_match() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("code.rs");
        std::fs::write(&file_path, "a a a").unwrap();

        let tool = EditFile;
        let output = tool
            .execute(&serde_json::json!({
                "path": file_path.to_str().unwrap(),
                "find": "a",
                "replace": "b"
            }))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("ambiguous"));
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "a a a");
    }

    #[tokio::test]
    async fn edit_file_rejects_missing_match() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("code.rs");
        std::fs::write(&file_path, "content").unwrap();

        let tool = EditFile;
        let output = tool
            .execute(&serde_json::json!({
                "path": file_path.to_str().unwrap(),
                "find": "not present",
                "replace": "x"
            }))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("not found"));
    }

    #[tokio::test]
    async fn bash_returns_stdout_and_success_exit_code() {
        let tool = Bash;
        let output = tool
            .execute(&serde_json::json!({"command": "echo hello_from_bash_tool"}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("hello_from_bash_tool"));
        assert!(output.content.contains("exit code: 0"));
    }

    #[tokio::test]
    async fn bash_reports_nonzero_exit_as_error_output() {
        let tool = Bash;
        let output = tool
            .execute(&serde_json::json!({"command": "exit 3"}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("exit code: 3"));
    }

    #[tokio::test]
    async fn grep_finds_matching_lines() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world\nfoo bar\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing here\n").unwrap();

        let tool = Grep;
        let output = tool
            .execute(&serde_json::json!({
                "pattern": "hello",
                "path": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("hello world"));
        assert!(!output.content.contains("nothing here"));
    }

    #[tokio::test]
    async fn grep_reports_no_matches() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "nothing relevant\n").unwrap();

        let tool = Grep;
        let output = tool
            .execute(&serde_json::json!({
                "pattern": "unmatchable_pattern_xyz",
                "path": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(output.content, "no matches found");
    }

    #[tokio::test]
    async fn glob_finds_matching_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("one.rs"), "").unwrap();
        std::fs::write(dir.path().join("two.txt"), "").unwrap();

        let tool = Glob;
        let output = tool
            .execute(&serde_json::json!({
                "pattern": "*.rs",
                "path": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("one.rs"));
        assert!(!output.content.contains("two.txt"));
    }
}
