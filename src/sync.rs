use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Args};
use indicatif::{ProgressBar, ProgressStyle};

use crate::api::ReadeckClient;
use crate::cache::Cache;
use crate::config::Config;
use crate::models::{Bookmark, BookmarkQuery, Filter, Sort};

const PAGE_SIZE: usize = 100;

const PROGRESS_TEMPLATE: &str = "{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len}";

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("source")
        .required(true)
        .multiple(true)
        .args(["unread", "favorites", "archived", "all"])
))]
pub struct SyncArgs {
    /// Download articles from the Unread list
    #[arg(long)]
    pub unread: bool,

    /// Download favorite bookmarks
    #[arg(long)]
    pub favorites: bool,

    /// Download archived bookmarks
    #[arg(long)]
    pub archived: bool,

    /// Download every bookmark
    #[arg(long)]
    pub all: bool,
}

pub async fn run(
    config: &Config,
    args: &SyncArgs,
    cache_dir: PathBuf,
) -> Result<()> {
    if !config.has_credentials() {
        bail!(
            "sync requires a server URL and API token; configure eckly or pass --server/--token"
        );
    }
    let client = ReadeckClient::new(&config.server, &config.token)
        .context("could not create Readeck client")?;
    let mut cache = match Cache::load(cache_dir.clone()) {
        Ok(cache) => cache,
        Err(err) => {
            eprintln!("Warning: {err:#}");
            Cache::new(cache_dir.clone())
        }
    };

    let interactive = std::io::stderr().is_terminal();
    upload_pending_changes(&client, &mut cache, interactive).await?;
    let list_progress = make_progress(0, interactive);
    let bookmarks =
        match download_bookmark_lists(&client, args, &list_progress).await {
            Ok(bookmarks) => {
                list_progress.finish_with_message(format!(
                    "Fetched {} bookmark(s)",
                    bookmarks.len()
                ));
                bookmarks
            }
            Err(err) => {
                list_progress.abandon_with_message("Failed to fetch bookmarks");
                return Err(err);
            }
        };
    let pending_updates = cache.pending_updates();
    let pending_deletes: HashSet<String> =
        cache.pending_deletes().into_iter().collect();
    let mut bookmarks = bookmarks;
    for bookmark in &mut bookmarks {
        if let Some(patch) = pending_updates.get(&bookmark.id) {
            bookmark.apply_patch(patch);
        }
    }
    bookmarks.retain(|bookmark| !pending_deletes.contains(&bookmark.id));
    cache.upsert_bookmarks(&bookmarks);
    if let Ok(labels) = client.labels().await {
        cache.set_labels(labels);
    }
    if let Ok(collections) = client.collections().await {
        cache.set_collections(collections);
    }
    cache.save().context("could not save bookmark cache")?;

    let articles: Vec<Bookmark> = bookmarks
        .into_iter()
        .filter(|bookmark| bookmark.has_article)
        .collect();
    let progress = make_progress(articles.len() as u64, interactive);

    if interactive && articles.is_empty() {
        eprintln!("No articles to sync.");
        return Ok(());
    }

    let mut errors = Vec::new();
    let mut synced = 0;
    progress.set_message("Downloading bookmarks...");
    for bookmark in articles {
        let title = bookmark.display_title();
        match client.article(&bookmark.id).await {
            Ok(html) => {
                match Cache::write_article_to_dir(
                    &cache_dir,
                    &bookmark.id,
                    &html,
                ) {
                    Ok(()) => synced += 1,
                    Err(err) => errors.push(format!("{title}: {err:#}")),
                }
            }
            Err(err) => errors.push(format!("{title}: {err}")),
        }
        progress.inc(1);
    }
    progress.finish_with_message(format!("Synced {synced} article(s)"));

    if !errors.is_empty() {
        for error in &errors {
            eprintln!("{error}");
        }
        bail!("{} article(s) failed to sync", errors.len());
    }
    Ok(())
}

fn make_progress(len: u64, interactive: bool) -> ProgressBar {
    if !interactive {
        return ProgressBar::hidden();
    }
    let progress = ProgressBar::new(len);
    progress.set_style(
        ProgressStyle::with_template(PROGRESS_TEMPLATE)
            .expect("valid progress template")
            .progress_chars("##-"),
    );
    progress
}

fn filters_for(args: &SyncArgs) -> Vec<Filter> {
    if args.all {
        return vec![Filter::All];
    }
    let mut filters = Vec::new();
    if args.unread {
        filters.push(Filter::Unread);
    }
    if args.favorites {
        filters.push(Filter::Marked);
    }
    if args.archived {
        filters.push(Filter::Archived);
    }
    filters
}

async fn upload_pending_changes(
    client: &ReadeckClient,
    cache: &mut Cache,
    interactive: bool,
) -> Result<()> {
    let updates = cache.pending_updates();
    let deletes = cache.pending_deletes();
    let total = updates.len() + deletes.len();
    if total == 0 {
        return Ok(());
    }

    let progress = make_progress(total as u64, interactive);
    progress.set_message("Uploading pending changes...");
    let mut uploaded = 0;
    let mut errors = Vec::new();
    let mut network_error = false;

    for (id, patch) in updates {
        match client.update_bookmark(&id, &patch).await {
            Ok(()) => {
                cache.remove_pending_update(&id);
                uploaded += 1;
            }
            Err(err) => {
                errors.push(format!("{id}: {err}"));
                if err.is_network() {
                    network_error = true;
                    break;
                }
            }
        }
        progress.inc(1);
    }

    if !network_error {
        for id in deletes {
            match client.delete_bookmark(&id).await {
                Ok(()) => {
                    cache.remove_pending_delete(&id);
                    uploaded += 1;
                }
                Err(err) if err.status() == Some(404) => {
                    cache.remove_pending_delete(&id);
                    uploaded += 1;
                }
                Err(err) => {
                    errors.push(format!("{id}: {err}"));
                    if err.is_network() {
                        break;
                    }
                }
            }
            progress.inc(1);
        }
    }

    cache.save().context("could not save bookmark cache")?;
    progress
        .finish_with_message(format!("Uploaded {uploaded} pending change(s)"));

    if !errors.is_empty() {
        for error in &errors {
            eprintln!("{error}");
        }
        bail!("{} pending change(s) failed to sync", errors.len());
    }
    Ok(())
}

async fn download_bookmark_lists(
    client: &ReadeckClient,
    args: &SyncArgs,
    progress: &ProgressBar,
) -> Result<Vec<Bookmark>> {
    let mut seen = HashSet::new();
    let mut bookmarks = Vec::new();
    let mut expected_total = 0u64;
    progress.set_message("Fetching bookmark list...");

    for filter in filters_for(args) {
        let mut offset = 0;
        loop {
            let query = BookmarkQuery::new(
                &filter,
                "",
                Sort::Newest,
                PAGE_SIZE,
                offset,
            );
            let page =
                client.list_bookmarks(&query).await.with_context(|| {
                    format!("could not list bookmarks for {}", filter.title())
                })?;
            if page.items.is_empty() {
                break;
            }
            let count = page.items.len();
            let filter_total = page.total.max(count);
            if offset == 0 {
                expected_total += filter_total as u64;
                progress.set_length(expected_total);
            }
            for bookmark in page.items {
                if seen.insert(bookmark.id.clone()) {
                    bookmarks.push(bookmark);
                }
            }
            progress.inc(count as u64);
            offset += count;
            if offset >= page.total {
                break;
            }
        }
    }

    Ok(bookmarks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(
        unread: bool,
        favorites: bool,
        archived: bool,
        all: bool,
    ) -> SyncArgs {
        SyncArgs {
            unread,
            favorites,
            archived,
            all,
        }
    }

    #[test]
    fn progress_template_is_valid() {
        ProgressStyle::with_template(PROGRESS_TEMPLATE).unwrap();
    }

    #[test]
    fn builds_filter_selection() {
        assert_eq!(
            filters_for(&args(true, false, false, false)),
            vec![Filter::Unread]
        );
        assert_eq!(
            filters_for(&args(true, true, false, false)),
            vec![Filter::Unread, Filter::Marked]
        );
        assert_eq!(
            filters_for(&args(false, false, true, true)),
            vec![Filter::All]
        );
        assert_eq!(
            filters_for(&args(false, false, true, false)),
            vec![Filter::Archived]
        );
    }
}
