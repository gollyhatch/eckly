use anyhow::{Context, Result};
use clap::Args;

use crate::api::ReadeckClient;
use crate::config::{Config, normalize_url};

#[derive(Debug, Args)]
pub struct AddArgs {
    /// URL of the bookmark to save
    pub url: String,

    /// Title for the bookmark (Readeck extracts one when omitted)
    pub title: Option<String>,
}

pub async fn run(config: &Config, args: &AddArgs) -> Result<()> {
    if !config.has_credentials() {
        anyhow::bail!(
            "add requires a server URL and API token; configure eckly or pass --server/--token"
        );
    }
    let url = normalize_url(&args.url).map_err(anyhow::Error::msg)?;
    let title = args
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty());
    let client = ReadeckClient::new(&config.server, &config.token)
        .context("could not create Readeck client")?;
    match client.create_bookmark(&url, title).await? {
        Some(id) => println!("Bookmark saved: {id}"),
        None => println!("Bookmark saved"),
    }
    Ok(())
}
