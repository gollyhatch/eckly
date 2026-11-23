use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::{ArgGroup, Args};
use indicatif::{ProgressBar, ProgressStyle};

use crate::api::{ApiError, ReadeckClient, SyncOptions};
use crate::cache::{Cache, content_signature};
use crate::config::Config;
use crate::models::{
    Bookmark, Filter, SyncChange, SyncChangeKind,
};

const ANNOTATION_PAGE_SIZE: usize = 100;
const METADATA_BATCH_SIZE: usize = 500;
const CONTENT_BATCH_SIZE: usize = 20;

const PROGRESS_TEMPLATE: &str = "{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len}";

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("source")
        .multiple(true)
        .args(["unread", "favorites", "archived", "all"])
))]
pub struct SyncArgs {
    /// Download Markdown and resources from the Unread list
    #[arg(long)]
    pub unread: bool,

    /// Download Markdown and resources for favorite bookmarks
    #[arg(long, alias = "favorite")]
    pub favorites: bool,

    /// Download Markdown and resources for archived bookmarks
    #[arg(long)]
    pub archived: bool,

    /// Download Markdown and resources for every bookmark
    #[arg(long)]
    pub all: bool,
}

impl SyncArgs {
    /// Lists whose bookmarks should have Markdown and resources downloaded.
    /// Metadata is always synced for every changed bookmark.
    fn content_filters(&self) -> Vec<Filter> {
        if self.all {
            return vec![Filter::All];
        }
        let mut filters = Vec::new();
        if self.unread {
            filters.push(Filter::Unread);
        }
        if self.favorites {
            filters.push(Filter::Marked);
        }
        if self.archived {
            filters.push(Filter::Archived);
        }
        filters
    }
}

/// Bookmarks changed since the last sync, plus deletions and the new
/// watermark.
#[derive(Debug, Default)]
pub struct MetadataBatch {
    pub updates: Vec<Bookmark>,
    pub removed: Vec<String>,
    pub watermark: Option<DateTime<Utc>>,
    pub snapshot: bool,
}

#[derive(Debug, Clone)]
pub struct ContentUpdate {
    pub id: String,
    pub signature: String,
}

#[derive(Debug, Default)]
pub struct ContentBatch {
    pub synced: Vec<ContentUpdate>,
    pub errors: Vec<String>,
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

    let progress = make_progress(0, interactive);
    progress.set_message("Fetching bookmark changes...");
    let batch = match fetch_metadata(&client, cache.last_sync()).await {
        Ok(batch) => batch,
        Err(err) => {
            progress.abandon_with_message("Failed to fetch changes");
            return Err(err).context("could not fetch changes from Readeck");
        }
    };
    progress.finish_and_clear();
    apply_metadata(&mut cache, batch)?;

    refresh_metadata(&client, &mut cache).await;
    cache.save().context("could not save bookmark cache")?;

    let filters = args.content_filters();
    if !filters.is_empty() {
        let targets: Vec<Bookmark> = cache
            .bookmarks()
            .into_iter()
            .filter(|bookmark| bookmark.has_article)
            .filter(|bookmark| {
                filters.iter().any(|filter| filter.matches(bookmark))
            })
            .filter(|bookmark| !cache.has_fresh_content(bookmark))
            .collect();
        let content =
            fetch_content(&client, cache.dir(), &targets, true, interactive)
                .await;
        for item in content.synced {
            cache.set_content(&item.id, item.signature);
        }
        cache.save().context("could not save bookmark cache")?;
        if !content.errors.is_empty() {
            for error in &content.errors {
                eprintln!("{error}");
            }
            bail!("{} content download(s) failed", content.errors.len());
        }
    }

    download_annotations(&client, &mut cache, interactive).await?;
    cache.save().context("could not save bookmark cache")?;
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

/// Fetch metadata for every bookmark that changed since the watermark.
///
/// With no watermark the change log is a complete snapshot of updated
/// bookmarks. Deletions are only available for incremental queries.
pub async fn fetch_metadata(
    client: &ReadeckClient,
    since: Option<DateTime<Utc>>,
) -> Result<MetadataBatch, ApiError> {
    let (changes, server_time) = client.sync_changes(since).await?;
    let watermark = server_time.or_else(|| changes.iter().map(|c| c.time).max());
    let changes = latest_changes(changes);

    let mut removed = Vec::new();
    let mut ids = Vec::new();
    for change in changes {
        match change.kind {
            SyncChangeKind::Delete => removed.push(change.id),
            SyncChangeKind::Update => ids.push(change.id),
        }
    }

    let mut updates = Vec::new();
    for chunk in ids.chunks(METADATA_BATCH_SIZE) {
        let options = SyncOptions {
            with_json: true,
            ..Default::default()
        };
        let items = client.sync_bookmarks(chunk, &options).await?;
        for item in items {
            if let Some(bookmark) = item.bookmark {
                updates.push(bookmark);
            }
        }
    }

    Ok(MetadataBatch {
        updates,
        removed,
        watermark,
        snapshot: since.is_none(),
    })
}

/// Merge a metadata batch into the cache and advance the sync watermark.
pub fn apply_metadata(cache: &mut Cache, batch: MetadataBatch) -> Result<()> {
    let pending_updates = cache.pending_updates();
    let pending_deletes: HashSet<String> =
        cache.pending_deletes().into_iter().collect();

    if batch.snapshot {
        let present: HashSet<&str> =
            batch.updates.iter().map(|b| b.id.as_str()).collect();
        let pending: HashSet<&str> =
            pending_updates.keys().map(String::as_str).collect();
        cache.retain_bookmarks(|id| present.contains(id) || pending.contains(id));
    }

    for id in &batch.removed {
        cache.remove_bookmark(id);
    }

    let mut updates = batch.updates;
    updates.retain(|bookmark| !pending_deletes.contains(&bookmark.id));
    for bookmark in &mut updates {
        if let Some(patch) = pending_updates.get(&bookmark.id) {
            bookmark.apply_patch(patch);
        }
    }
    cache.upsert_bookmarks(&updates);
    if let Some(watermark) = batch.watermark {
        cache.set_last_sync(watermark);
    }
    cache.save().context("could not save bookmark cache")
}

/// Download Markdown, resources and an optional HTML sidecar for the given
/// bookmarks. The HTML sidecar is used only for annotation selectors.
pub async fn fetch_content(
    client: &ReadeckClient,
    cache_dir: &Path,
    bookmarks: &[Bookmark],
    with_html: bool,
    interactive: bool,
) -> ContentBatch {
    let targets: Vec<&Bookmark> =
        bookmarks.iter().filter(|bookmark| bookmark.has_article).collect();
    let progress = make_progress(targets.len() as u64, interactive);
    progress.set_message("Downloading bookmark content...");
    let mut batch = ContentBatch::default();

    for chunk in targets.chunks(CONTENT_BATCH_SIZE) {
        let ids: Vec<String> =
            chunk.iter().map(|bookmark| bookmark.id.clone()).collect();
        let options = SyncOptions {
            with_json: false,
            with_html,
            with_markdown: true,
            with_resources: true,
            resource_prefix: ".".into(),
        };
        let items = match client.sync_bookmarks(&ids, &options).await {
            Ok(items) => items,
            Err(err) => {
                batch
                    .errors
                    .push(format!("content sync failed: {err}"));
                progress.inc(chunk.len() as u64);
                continue;
            }
        };
        let by_id: HashMap<&str, &Bookmark> =
            chunk.iter().map(|b| (b.id.as_str(), *b)).collect();
        for item in items {
            let Some(bookmark) = by_id.get(item.id.as_str()) else {
                continue;
            };
            let Some(markdown) = item.markdown else {
                batch
                    .errors
                    .push(format!("{}: no markdown returned", item.id));
                continue;
            };
            if let Err(err) = Cache::remove_content_from_dir(cache_dir, &item.id)
            {
                batch.errors.push(format!("{}: {err:#}", item.id));
                continue;
            }
            if let Err(err) = Cache::write_markdown_to_dir(
                cache_dir,
                &item.id,
                &markdown,
            ) {
                batch.errors.push(format!("{}: {err:#}", item.id));
                continue;
            }
            let mut failed = false;
            for resource in item.resources {
                if let Err(err) = Cache::write_resource_to_dir(
                    cache_dir,
                    &item.id,
                    &resource.path,
                    &resource.bytes,
                ) {
                    batch.errors.push(format!(
                        "{}: resource {}: {err:#}",
                        item.id, resource.path
                    ));
                    failed = true;
                }
            }
            if let Some(html) = item.html {
                if let Err(err) = Cache::write_article_to_dir(
                    cache_dir,
                    &item.id,
                    &html,
                ) {
                    batch.errors.push(format!("{}: {err:#}", item.id));
                    failed = true;
                }
            }
            if !failed {
                batch.synced.push(ContentUpdate {
                    id: item.id,
                    signature: content_signature(bookmark),
                });
            }
        }
        progress.inc(chunk.len() as u64);
    }
    progress.finish_with_message(format!(
        "Downloaded content for {} bookmark(s)",
        batch.synced.len()
    ));
    batch
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

/// Download annotations for every annotated bookmark and drop stale ones.
async fn download_annotations(
    client: &ReadeckClient,
    cache: &mut Cache,
    interactive: bool,
) -> Result<()> {
    let mut annotated: HashSet<String> = HashSet::new();
    let mut offset = 0;
    loop {
        let page = client
            .annotations_page(ANNOTATION_PAGE_SIZE, offset)
            .await
            .context("could not list annotations")?;
        if page.items.is_empty() {
            break;
        }
        let count = page.items.len();
        for annotation in page.items {
            if !annotation.bookmark_id.is_empty() {
                annotated.insert(annotation.bookmark_id);
            }
        }
        offset += count;
        if offset >= page.total {
            break;
        }
    }

    let known: HashSet<String> = cache.bookmark_ids().into_iter().collect();
    let annotated: HashSet<String> = annotated
        .into_iter()
        .filter(|id| known.contains(id))
        .collect();

    let progress = make_progress(annotated.len() as u64, interactive);
    progress.set_message("Downloading annotations...");
    let mut synced = 0usize;
    let mut errors = Vec::new();
    for bookmark_id in &annotated {
        match client.annotations(bookmark_id).await {
            Ok(annotations) => {
                synced += annotations.len();
                cache.set_annotations(bookmark_id, annotations);
            }
            Err(err) => errors.push(format!("{bookmark_id}: {err}")),
        }
        progress.inc(1);
    }
    for bookmark_id in &known {
        if !annotated.contains(bookmark_id) {
            cache.set_annotations(bookmark_id, Vec::new());
        }
    }
    progress.finish_with_message(format!("Synced {synced} annotation(s)"));

    if !errors.is_empty() {
        for error in &errors {
            eprintln!("{error}");
        }
        bail!("{} annotation list(s) failed to sync", errors.len());
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Annotation, BookmarkPatch};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn fetch_metadata_uses_the_change_log_and_sync_endpoint() {
        let changes = r#"[
            {"id":"one","time":"2026-11-23T10:00:00Z","type":"update"},
            {"id":"gone","time":"2026-11-23T10:01:00Z","type":"delete"}
        ]"#;
        let multipart = multipart_body(&[
            ("json", "", "{\"id\":\"one\",\"title\":\"Fetched\",\"word_count\":3}"),
        ]);
        let (base, requests) = serve(vec![
            http_response(
                "Date: Mon, 23 Nov 2026 10:02:00 GMT\r\n",
                changes,
            ),
            multipart_response(&multipart),
        ])
        .await;
        let client = ReadeckClient::new(&base, "secret-token").unwrap();
        let since = DateTime::parse_from_rfc3339("2026-11-23T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let batch = fetch_metadata(&client, Some(since)).await.unwrap();
        assert!(!batch.snapshot);
        assert_eq!(batch.removed, vec!["gone".to_string()]);
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].display_title(), "Fetched");
        assert_eq!(
            batch.watermark.unwrap().to_rfc3339(),
            "2026-11-23T10:02:00+00:00"
        );
        let requests = requests.await.unwrap();
        assert!(requests[0].starts_with("GET /api/bookmarks/sync?since="));
        assert!(requests[1].starts_with("POST /api/bookmarks/sync "));
        assert!(requests[1].contains("\"with_json\":true"));
    }

    #[tokio::test]
    async fn fetch_content_caches_markdown_resources_and_html() {
        let dir = std::env::temp_dir()
            .join(format!("eckly-sync-content-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let multipart = multipart_body(&[
            ("markdown", "", "# Hello\n\n![pic](pic.png)\n"),
            ("html", "", "<h1>Hello</h1>"),
            ("resource", "Path: pic.png\r\nGroup: embedded\r\n", "PNG"),
        ]);
        let (base, _requests) =
            serve(vec![multipart_response(&multipart)]).await;
        let client = ReadeckClient::new(&base, "secret-token").unwrap();
        let bookmark = Bookmark {
            id: "one".into(),
            has_article: true,
            loaded: true,
            state: 1,
            word_count: 2,
            updated: Some(Utc::now()),
            ..Default::default()
        };
        let batch =
            fetch_content(&client, &dir, &[bookmark], true, false).await;
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        assert_eq!(batch.synced.len(), 1);
        assert_eq!(
            Cache::read_markdown_from_dir(&dir, "one").as_deref(),
            Some("# Hello\n\n![pic](pic.png)\n")
        );
        assert_eq!(
            Cache::read_article_from_dir(&dir, "one").as_deref(),
            Some("<h1>Hello</h1>")
        );
        let cache = Cache::new(dir.clone());
        assert_eq!(
            cache.read_resource("one", "pic.png").as_deref(),
            Some(b"PNG" as &[u8])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn multipart_body(parts: &[(&str, &str, &str)]) -> String {
        let mut body = String::new();
        for (kind, headers, content) in parts {
            body.push_str("--eckly-boundary\r\n");
            body.push_str("Bookmark-Id: one\r\n");
            body.push_str(&format!("Type: {kind}\r\n"));
            body.push_str(headers);
            body.push_str("\r\n");
            body.push_str(content);
            body.push_str("\r\n");
        }
        body.push_str("--eckly-boundary--\r\n");
        body
    }

    fn http_response(headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        )
    }

    fn multipart_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: multipart/mixed; boundary=\"eckly-boundary\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    async fn serve(
        responses: Vec<String>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut header_end = None;
                loop {
                    let mut chunk = [0u8; 1024];
                    let read = socket.read(&mut chunk).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if header_end.is_none() {
                        header_end = find_header_end(&request);
                    }
                    if let Some(end) = header_end {
                        let length = content_length(&request[..end]).unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests
                    .push(String::from_utf8_lossy(&request).to_string());
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.ok();
            }
            requests
        });
        (format!("http://{address}"), handle)
    }

    fn find_header_end(bytes: &[u8]) -> Option<usize> {
        bytes.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn content_length(headers: &[u8]) -> Option<usize> {
        let headers = String::from_utf8_lossy(headers);
        headers.lines().find_map(|line| {
            line.strip_prefix("Content-Length:")
                .or_else(|| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())
        })
    }

    #[test]
    fn progress_template_is_valid() {
        ProgressStyle::with_template(PROGRESS_TEMPLATE).unwrap();
    }

    #[test]
    fn content_flags_select_lists() {
        let args = |unread, favorites, archived, all| SyncArgs {
            unread,
            favorites,
            archived,
            all,
        };
        assert!(args(false, false, false, false)
            .content_filters()
            .is_empty());
        assert_eq!(
            args(true, false, false, false).content_filters(),
            vec![Filter::Unread]
        );
        assert_eq!(
            args(false, true, false, false).content_filters(),
            vec![Filter::Marked]
        );
        assert_eq!(
            args(false, false, true, false).content_filters(),
            vec![Filter::Archived]
        );
        assert_eq!(
            args(false, false, false, true).content_filters(),
            vec![Filter::All]
        );
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
        assert_eq!(changes[0].id, "two");
        assert_eq!(changes[1].id, "one");
        assert_eq!(changes[1].kind, SyncChangeKind::Delete);
        assert_eq!(changes[1].time.to_rfc3339(), "2026-11-14T11:00:00+00:00");
    }

    #[test]
    fn metadata_batch_applies_updates_deletes_and_pending_patches() {
        let dir = std::env::temp_dir()
            .join(format!("eckly-sync-meta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cache = Cache::new(dir.clone());
        cache.upsert_bookmarks(&[
            Bookmark {
                id: "keep".into(),
                title: Some("Old".into()),
                ..Default::default()
            },
            Bookmark {
                id: "gone".into(),
                ..Default::default()
            },
            Bookmark {
                id: "stale".into(),
                ..Default::default()
            },
        ]);
        cache.queue_patch(
            "keep",
            &BookmarkPatch {
                title: Some("Local".into()),
                ..Default::default()
            },
        );
        cache.set_annotations("gone", vec![Annotation::default()]);

        let watermark = DateTime::parse_from_rfc3339("2026-11-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        apply_metadata(
            &mut cache,
            MetadataBatch {
                updates: vec![Bookmark {
                    id: "keep".into(),
                    title: Some("Remote".into()),
                    ..Default::default()
                }],
                removed: vec!["gone".into()],
                watermark: Some(watermark),
                snapshot: true,
            },
        )
        .unwrap();

        assert_eq!(cache.bookmark_ids(), vec!["keep".to_string()]);
        assert_eq!(
            cache.bookmark_ref("keep").unwrap().title.as_deref(),
            Some("Local")
        );
        assert_eq!(cache.last_sync(), Some(watermark));
        assert!(!dir.join("content").join("gone.md").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
