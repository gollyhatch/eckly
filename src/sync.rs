use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::{ArgGroup, Args};
use indicatif::{ProgressBar, ProgressStyle};

use crate::api::ReadeckClient;
use crate::cache::Cache;
use crate::config::Config;
use crate::models::{
    Bookmark, BookmarkQuery, Filter, Sort, SyncChange, SyncChangeKind,
};

const PAGE_SIZE: usize = 100;

const PROGRESS_TEMPLATE: &str = "{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len}";

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("source")
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

impl SyncArgs {
    /// Without a list flag, sync only what changed since the last sync.
    fn incremental(&self) -> bool {
        !self.unread && !self.favorites && !self.archived && !self.all
    }
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

    if !args.incremental() {
        let filters = filters_for(args);
        return sync_lists(
            &client,
            &mut cache,
            &cache_dir,
            filters,
            interactive,
        )
        .await;
    }

    match cache.last_sync() {
        Some(since) => {
            sync_changes(&client, &mut cache, &cache_dir, since, interactive)
                .await
        }
        // Without a watermark there is nothing to diff against, so take a
        // complete snapshot instead.
        None => {
            sync_lists(
                &client,
                &mut cache,
                &cache_dir,
                vec![Filter::All],
                interactive,
            )
            .await
        }
    }
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
    if args.all || args.incremental() {
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

/// Only a sync of every list yields a cache complete enough to advance the
/// incremental watermark.
fn is_complete_snapshot(filters: &[Filter]) -> bool {
    filters.len() == 1 && filters[0] == Filter::All
}

/// Download the requested lists, cache their metadata and fetch new or
/// changed article bodies.
async fn sync_lists(
    client: &ReadeckClient,
    cache: &mut Cache,
    cache_dir: &Path,
    filters: Vec<Filter>,
    interactive: bool,
) -> Result<()> {
    let complete = is_complete_snapshot(&filters);
    // Queried before the snapshot so the next incremental run cannot miss a
    // change that lands while the list is being paginated.
    let (changed, watermark) = match cache.last_sync() {
        Some(since) => {
            let (changed, newest) = changes_since(client, since).await;
            (changed, complete.then(|| newest.unwrap_or(since)))
        }
        None => {
            let newest = if complete {
                snapshot_watermark(client).await
            } else {
                None
            };
            (HashSet::new(), newest)
        }
    };
    let list_progress = make_progress(0, interactive);
    let bookmarks =
        match download_bookmark_lists(client, &filters, &list_progress).await {
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
    if complete {
        let present: HashSet<&str> =
            bookmarks.iter().map(|b| b.id.as_str()).collect();
        cache.retain_bookmarks(|id| present.contains(id));
    }
    let bookmarks = merge_pending(cache, bookmarks);
    cache.upsert_bookmarks(&bookmarks);
    refresh_metadata(client, cache).await;
    cache.save().context("could not save bookmark cache")?;

    let articles: Vec<Bookmark> = bookmarks
        .into_iter()
        .filter(|bookmark| needs_article(bookmark, &changed, cache))
        .collect();
    download_articles(client, cache_dir, articles, interactive).await?;

    if let Some(watermark) = watermark {
        cache.set_last_sync(watermark);
        cache.save().context("could not save bookmark cache")?;
    }
    Ok(())
}

/// An article is fetched when it is missing locally, or when its bookmark
/// changed on the server since the last sync.
fn needs_article(
    bookmark: &Bookmark,
    changed: &HashSet<String>,
    cache: &Cache,
) -> bool {
    bookmark.has_article
        && (changed.contains(&bookmark.id) || !cache.has_article(&bookmark.id))
}

/// Bookmark IDs updated since `since`, plus the server's time at response.
///
/// Best effort: on failure the caller keeps its existing watermark and
/// re-fetches only missing articles.
async fn changes_since(
    client: &ReadeckClient,
    since: DateTime<Utc>,
) -> (HashSet<String>, Option<DateTime<Utc>>) {
    match client.sync_changes(Some(since)).await {
        Ok((changes, time)) => {
            let changed = changes
                .iter()
                .filter(|change| change.kind == SyncChangeKind::Update)
                .map(|change| change.id.clone())
                .collect();
            let time =
                time.or_else(|| changes.iter().map(|change| change.time).max());
            (changed, time)
        }
        Err(_) => (HashSet::new(), None),
    }
}

/// The server's newest change time, used as the starting watermark for an
/// incremental sync after a complete snapshot.
///
/// Best effort: servers without the change log simply never sync
/// incrementally.
async fn snapshot_watermark(client: &ReadeckClient) -> Option<DateTime<Utc>> {
    let (changes, time) = client.sync_changes(None).await.ok()?;
    time.or_else(|| changes.iter().map(|change| change.time).max())
}

/// Apply only what the server reports as changed since the last sync.
async fn sync_changes(
    client: &ReadeckClient,
    cache: &mut Cache,
    cache_dir: &Path,
    since: DateTime<Utc>,
    interactive: bool,
) -> Result<()> {
    let progress = make_progress(0, interactive);
    progress.set_message("Fetching changes...");
    let (changes, server_time) = match client.sync_changes(Some(since)).await {
        Ok(result) => result,
        Err(err) => {
            progress.abandon_with_message("Failed to fetch changes");
            return Err(err).context("could not fetch changes from Readeck");
        }
    };
    // The server's clock at response time is the natural next `since`: it
    // covers everything in this response without repeating it forever.
    let watermark = server_time.unwrap_or_else(|| {
        changes
            .iter()
            .map(|change| change.time)
            .max()
            .unwrap_or(since)
    });
    let changes = latest_changes(changes);
    progress.set_length(changes.len() as u64);
    progress.set_message("Fetching changed bookmarks...");

    let pending_updates = cache.pending_updates();
    let mut fetched = Vec::new();
    let mut removed = 0;
    let mut errors = Vec::new();

    for change in changes {
        match change.kind {
            SyncChangeKind::Delete => {
                cache.remove_bookmark(&change.id);
                removed += 1;
            }
            SyncChangeKind::Update => {
                match client.bookmark(&change.id).await {
                    Ok(mut bookmark) => {
                        if let Some(patch) = pending_updates.get(&bookmark.id) {
                            bookmark.apply_patch(patch);
                        }
                        fetched.push(bookmark);
                    }
                    // Deleted between the change log and this request.
                    Err(err) if err.status() == Some(404) => {
                        cache.remove_bookmark(&change.id);
                        removed += 1;
                    }
                    Err(err) => errors.push(format!("{}: {err}", change.id)),
                }
            }
        }
        progress.inc(1);
    }
    progress.finish_with_message(format!(
        "Fetched {} changed bookmark(s), removed {removed}",
        fetched.len()
    ));

    // Everything here changed on the server since the last sync, so refresh
    // its article too, even when a copy is already cached.
    let articles: Vec<Bookmark> = fetched
        .iter()
        .filter(|bookmark| bookmark.has_article)
        .cloned()
        .collect();
    cache.upsert_bookmarks(&fetched);
    refresh_metadata(client, cache).await;
    cache.save().context("could not save bookmark cache")?;

    if !errors.is_empty() {
        for error in &errors {
            eprintln!("{error}");
        }
        bail!("{} change(s) failed to sync", errors.len());
    }

    download_articles(client, cache_dir, articles, interactive).await?;

    // Only advance the watermark once every change was applied; otherwise the
    // next run would skip whatever failed here.
    cache.set_last_sync(watermark);
    cache.save().context("could not save bookmark cache")?;
    Ok(())
}

/// Keep the newest entry per bookmark, oldest first.
fn latest_changes(changes: Vec<SyncChange>) -> Vec<SyncChange> {
    let mut latest: HashMap<String, SyncChange> = HashMap::new();
    for change in changes {
        match latest.get(&change.id) {
            Some(existing) if existing.time >= change.time => {}
            _ => {
                latest.insert(change.id.clone(), change);
            }
        }
    }
    let mut changes: Vec<SyncChange> = latest.into_values().collect();
    changes.sort_by_key(|change| change.time);
    changes
}

async fn refresh_metadata(client: &ReadeckClient, cache: &mut Cache) {
    if let Ok(labels) = client.labels().await {
        cache.set_labels(labels);
    }
    if let Ok(collections) = client.collections().await {
        cache.set_collections(collections);
    }
}

/// Keep local edits that have not reached the server yet.
fn merge_pending(cache: &Cache, bookmarks: Vec<Bookmark>) -> Vec<Bookmark> {
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
    bookmarks
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
    filters: &[Filter],
    progress: &ProgressBar,
) -> Result<Vec<Bookmark>> {
    let mut seen = HashSet::new();
    let mut bookmarks = Vec::new();
    let mut expected_total = 0u64;
    progress.set_message("Fetching bookmark list...");

    for filter in filters {
        let mut offset = 0;
        loop {
            let query =
                BookmarkQuery::new(filter, "", Sort::Newest, PAGE_SIZE, offset);
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

async fn download_articles(
    client: &ReadeckClient,
    cache_dir: &Path,
    bookmarks: Vec<Bookmark>,
    interactive: bool,
) -> Result<()> {
    let articles: Vec<Bookmark> = bookmarks
        .into_iter()
        .filter(|bookmark| bookmark.has_article)
        .collect();
    if interactive && articles.is_empty() {
        eprintln!("No articles to sync.");
        return Ok(());
    }

    let progress = make_progress(articles.len() as u64, interactive);
    let mut errors = Vec::new();
    let mut synced = 0;
    progress.set_message("Downloading bookmarks...");
    for bookmark in articles {
        let title = bookmark.display_title();
        match client.article(&bookmark.id).await {
            Ok(html) => {
                match Cache::write_article_to_dir(
                    cache_dir,
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

    #[test]
    fn no_flags_selects_the_whole_library() {
        let incremental = args(false, false, false, false);
        assert!(incremental.incremental());
        assert_eq!(filters_for(&incremental), vec![Filter::All]);
        assert!(is_complete_snapshot(&filters_for(&incremental)));
        assert!(!is_complete_snapshot(&filters_for(&args(
            true, false, false, false
        ))));
    }

    #[test]
    fn parses_the_change_log() {
        let changes: Vec<SyncChange> = serde_json::from_str(
            r#"[
                {"id":"one","time":"2026-11-14T21:18:09.039Z","type":"delete"},
                {"id":"two","time":"2026-11-14T21:18:08.123456789Z","type":"update"}
            ]"#,
        )
        .unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].id, "one");
        assert_eq!(changes[0].kind, SyncChangeKind::Delete);
        assert_eq!(changes[1].kind, SyncChangeKind::Update);
        assert_eq!(
            changes[1].time.to_rfc3339(),
            "2026-11-14T21:18:08.123456789+00:00"
        );
    }

    #[test]
    fn keeps_only_the_latest_change_per_bookmark() {
        let changes: Vec<SyncChange> = serde_json::from_str(
            r#"[
                {"id":"one","time":"2026-11-14T10:00:00Z","type":"update"},
                {"id":"one","time":"2026-11-14T11:00:00Z","type":"delete"},
                {"id":"two","time":"2026-11-14T09:00:00Z","type":"update"}
            ]"#,
        )
        .unwrap();
        let changes = latest_changes(changes);
        assert_eq!(changes.len(), 2);
        // Sorted by time: two, then the newest change for one.
        assert_eq!(changes[0].id, "two");
        assert_eq!(changes[1].id, "one");
        assert_eq!(changes[1].kind, SyncChangeKind::Delete);
        assert_eq!(changes[1].time.to_rfc3339(), "2026-11-14T11:00:00+00:00");
    }

    #[test]
    fn fetches_only_missing_or_changed_articles() {
        let dir = std::env::temp_dir()
            .join(format!("eckly-sync-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = Cache::new(dir.clone());
        cache.write_article("stable", "<p>stable</p>").unwrap();
        cache.write_article("touched", "<p>old</p>").unwrap();

        let bookmark = |id: &str, has_article: bool| Bookmark {
            id: id.into(),
            has_article,
            ..Default::default()
        };
        let changed: HashSet<String> =
            ["touched".to_string()].into_iter().collect();

        // Cached and unchanged: keep the local copy.
        assert!(!needs_article(&bookmark("stable", true), &changed, &cache));
        // Cached but changed on the server: refresh it.
        assert!(needs_article(&bookmark("touched", true), &changed, &cache));
        // Never downloaded: fetch it.
        assert!(needs_article(&bookmark("new", true), &changed, &cache));
        // Nothing to download.
        assert!(!needs_article(&bookmark("new", false), &changed, &cache));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
