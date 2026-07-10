// src/permissions/stdio.rs

use std::future::Future;
use std::pin::Pin;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::permissions::types::{PermissionDecision, PermissionPrompter, PermissionRequest};

/// Renders a [`PermissionRequest`] as plain numbered choices over any
/// `AsyncBufRead`/`AsyncWrite` pair (real stdin/stdout in production, an in-memory
/// buffer in tests). The TUI phase will supply a different [`PermissionPrompter`]
/// impl that renders inline in the transcript instead — this type is not reused
/// there, but the trait it implements is.
pub struct StdioPrompter<R, W> {
    input: Mutex<R>,
    output: Mutex<W>,
}

impl<R, W> StdioPrompter<R, W>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    pub fn new(input: R, output: W) -> Self {
        Self {
            input: Mutex::new(input),
            output: Mutex::new(output),
        }
    }
}

impl StdioPrompter<tokio::io::BufReader<tokio::io::Stdin>, tokio::io::Stdout> {
    /// Convenience constructor wired to the real process stdin/stdout.
    pub fn real() -> Self {
        Self::new(
            tokio::io::BufReader::new(tokio::io::stdin()),
            tokio::io::stdout(),
        )
    }
}

impl<R, W> PermissionPrompter for StdioPrompter<R, W>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    fn prompt<'a>(
        &'a self,
        request: &'a PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
        Box::pin(async move {
            let mut out = self.output.lock().await;
            let _ = out
                .write_all(
                    format!(
                        "\nPermission requested: {}\n  1) Yes\n  2) Yes, don't ask again this session\n  3) No (provide feedback)\n> ",
                        request.description
                    )
                    .as_bytes(),
                )
                .await;
            let _ = out.flush().await;
            drop(out);

            let mut input = self.input.lock().await;
            let mut line = String::new();
            if input.read_line(&mut line).await.is_err() {
                return PermissionDecision::Deny {
                    feedback: "failed to read permission response".into(),
                };
            }

            match line.trim() {
                "1" => PermissionDecision::Allow,
                "2" => PermissionDecision::AllowAlwaysThisSession,
                _ => {
                    let mut out = self.output.lock().await;
                    let _ = out
                        .write_all(b"Feedback (why not / what to do instead): ")
                        .await;
                    let _ = out.flush().await;
                    drop(out);

                    let mut feedback = String::new();
                    let _ = input.read_line(&mut feedback).await;
                    PermissionDecision::Deny {
                        feedback: feedback.trim().to_string(),
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn request() -> PermissionRequest {
        PermissionRequest {
            tool_name: "bash".into(),
            description: "run shell command: rm file.txt".into(),
            command_preview: Some("rm file.txt".into()),
        }
    }

    #[tokio::test]
    async fn choice_1_allows() {
        let prompter = StdioPrompter::new(
            tokio::io::BufReader::new(Cursor::new(b"1\n".to_vec())),
            Vec::new(),
        );
        let decision = prompter.prompt(&request()).await;
        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[tokio::test]
    async fn choice_2_allows_always_this_session() {
        let prompter = StdioPrompter::new(
            tokio::io::BufReader::new(Cursor::new(b"2\n".to_vec())),
            Vec::new(),
        );
        let decision = prompter.prompt(&request()).await;
        assert_eq!(decision, PermissionDecision::AllowAlwaysThisSession);
    }

    #[tokio::test]
    async fn choice_3_denies_with_feedback() {
        let prompter = StdioPrompter::new(
            tokio::io::BufReader::new(Cursor::new(b"3\nplease use a temp file instead\n".to_vec())),
            Vec::new(),
        );
        let decision = prompter.prompt(&request()).await;
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                feedback: "please use a temp file instead".into()
            }
        );
    }

    #[tokio::test]
    async fn unrecognized_input_treated_as_deny() {
        let prompter = StdioPrompter::new(
            tokio::io::BufReader::new(Cursor::new(b"garbage\nbecause reasons\n".to_vec())),
            Vec::new(),
        );
        let decision = prompter.prompt(&request()).await;
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                feedback: "because reasons".into()
            }
        );
    }

    #[tokio::test]
    async fn prompt_text_is_written_to_output() {
        let output = Vec::new();
        let prompter = StdioPrompter::new(
            tokio::io::BufReader::new(Cursor::new(b"1\n".to_vec())),
            output,
        );
        prompter.prompt(&request()).await;
        let written = prompter.output.lock().await;
        let text = String::from_utf8(written.clone()).unwrap();
        assert!(text.contains("run shell command: rm file.txt"));
        assert!(text.contains("Yes, don't ask again this session"));
    }
}
