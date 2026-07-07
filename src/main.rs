use clap::Parser;
use local_code::cli::{run, Cli};

/// Hidden CLI mode that turns this binary into the stdio MCP test fixture
/// (see `local_code::mcp::fixture_server`), so integration tests can spawn a
/// real child process without a second `[[bin]]` target. Not documented,
/// not part of `Cli`'s clap surface.
const MCP_FIXTURE_SERVER_ARG: &str = "__mcp_fixture_server";

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some(MCP_FIXTURE_SERVER_ARG) {
        local_code::mcp::fixture_server::run();
        return Ok(());
    }

    let cli = Cli::parse();
    let project_root = std::env::current_dir()?;
    run(cli, project_root).await
}
