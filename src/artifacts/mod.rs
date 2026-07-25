//! Localhost HTTP serving of agent-created artifacts (HTML/CSS/JS mockups,
//! images, reports) so the agent can showcase its work in the user's browser
//! and gather feedback on visual designs.
//!
//! [`server`] is a hand-rolled HTTP/1.1 static-file server bound to
//! 127.0.0.1 on an OS-assigned port — no new dependencies, tokio only.
//! [`tool`] is the `serve_artifacts` built-in tool that starts (or reuses)
//! the server for `<project>/.local-code/artifacts/` and returns the base
//! URL; the agent then drops files there with `write_file` and shares the
//! links.

pub mod server;
pub mod tool;
