use clap::Parser;
use local_code::cli::{run, Cli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let project_root = std::env::current_dir()?;
    run(cli, project_root).await
}
