use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::models::{
    Annotation, Bookmark, Collection, Filter, Label, Profile, SearchQuery, Sort,
};

const VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct CacheState {
    #[serde(default = "cache_version")]
    version: u32,
    #[serde(default)]
    bookmarks: HashMap<String, Bookmark>,
    #[serde(default)]
    labels: Vec<Label>,
    #[serde(default)]
    collections: Vec<Collection>,
    #[serde(default)]
    profile: Option<Profile>,
    #[serde(default)]
    pending: PendingChanges,
    /// Annotations downloaded by the last sync, by bookmark.
    #[serde(default)]
    annotations: HashMap<String, Vec<Annotation>>,
    /// Server time of the newest change covered by the last incremental sync.
    #[serde(default)]
    last_sync: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PendingChanges {
    #[serde(default)]
    updates: HashMap<String, crate::models::BookmarkPatch>,
    #[serde(default)]
    deletes: std::collections::HashSet<String>,
}

fn cache_version() -> u32 {
    VERSION
}

impl CacheState {
    fn empty() -> Self {
        Self {
            version: VERSION,
            bookmarks: HashMap::new(),
            labels: Vec::new(),
            collections: Vec::new(),
            profile: None,
            pending: PendingChanges::default(),
            annotations: HashMap::new(),
            last_sync: None,
        }
    }
}

fn merge_patch(
    target: &mut crate::models::BookmarkPatch,
    incoming: &crate::models::BookmarkPatch,
) {
    if incoming.title.is_some() {
        target.title = incoming.title.clone();
    }
    if incoming.description.is_some() {
        target.description = incoming.description.clone();
    }
    if incoming.note.is_some() {
        target.note = incoming.note.clone();
    }
    if incoming.is_marked.is_some() {
        target.is_marked = incoming.is_marked;
    }
    if incoming.is_archived.is_some() {
        target.is_archived = incoming.is_archived;
    }
    if incoming.is_deleted.is_some() {
        target.is_deleted = incoming.is_deleted;
    }
    if incoming.read_progress.is_some() {
        target.read_progress = incoming.read_progress;
    }
    if let Some(labels) = &incoming.labels {
        target.labels = Some(labels.clone());
        target.add_labels = None;
        target.remove_labels = None;
    }
    if let Some(labels) = &incoming.add_labels {
        target
            .add_labels
            .get_or_insert_with(Vec::new)
            .extend(labels.iter().cloned());
    }
    if let Some(labels) = &incoming.remove_labels {
        target
            .remove_labels
            .get_or_insert_with(Vec::new)
            .extend(labels.iter().cloned());
    }
}

/// Persistent cache of bookmark metadata and article HTML.
#[derive(Debug)]
pub struct Cache {
    dir: PathBuf,
    state: CacheState,
}

impl Cache {
    pub fn load(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir).with_context(|| {
            format!("could not create cache dir {}", dir.display())
        })?;
        let state_path = dir.join("state.json");
        let state = if state_path.exists() {
            let data = fs::read_to_string(&state_path).with_context(|| {
                format!("could not read cache file {}", state_path.display())
            })?;
            let state: CacheState =
                serde_json::from_str(&data).with_context(|| {
                    format!(
                        "could not parse cache file {}",
                        state_path.display()
                    )
                })?;
            if state.version == VERSION {
                state
            } else {
                CacheState::empty()
            }
        } else {
            CacheState::empty()
        };
        Ok(Self { dir, state })
    }

    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            state: CacheState::empty(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(&self.dir).with_context(|| {
            format!("could not create cache dir {}", self.dir.display())
        })?;
        let target = self.dir.join("state.json");
        let temporary = self.dir.join("state.json.tmp");
        let data = serde_json::to_vec_pretty(&self.state)
            .context("could not serialize bookmark cache")?;
        fs::write(&temporary, data).with_context(|| {
            format!("could not write cache file {}", temporary.display())
        })?;
        fs::rename(&temporary, &target).with_context(|| {
            format!("could not replace cache file {}", target.display())
        })?;
        Ok(())
    }

    pub fn labels(&self) -> Vec<Label> {
        self.state.labels.clone()
    }

    pub fn collections(&self) -> Vec<Collection> {
        self.state.collections.clone()
    }

    pub fn profile(&self) -> Option<Profile> {
        self.state.profile.clone()
    }

    pub fn set_labels(&mut self, labels: Vec<Label>) {
        self.state.labels = labels;
    }

    /// Rename a label in cached labels and bookmarks, merging duplicates.
    pub fn rename_label(&mut self, from: &str, to: &str) {
        let mut labels: Vec<Label> =
            Vec::with_capacity(self.state.labels.len());
        for mut label in self.state.labels.drain(..) {
            if label.name == from {
                label.name = to.to_string();
            }
            match labels.iter_mut().find(|kept| kept.name == label.name) {
                Some(existing) => existing.count += label.count,
                None => labels.push(label),
            }
        }
        self.state.labels = labels;

        for bookmark in self.state.bookmarks.values_mut() {
            for label in &mut bookmark.labels {
                if label == from {
                    *label = to.to_string();
                }
            }
            let mut seen = std::collections::HashSet::new();
            bookmark.labels.retain(|label| seen.insert(label.clone()));
        }
    }

    pub fn set_collections(&mut self, collections: Vec<Collection>) {
        self.state.collections = collections;
    }

    pub fn set_profile(&mut self, profile: Profile) {
        self.state.profile = Some(profile);
    }

    pub fn upsert_bookmarks(&mut self, bookmarks: &[Bookmark]) {
        for bookmark in bookmarks {
            self.state
                .bookmarks
                .insert(bookmark.id.clone(), bookmark.clone());
        }
    }

    pub fn annotations(&self, bookmark_id: &str) -> Vec<Annotation> {
        self.state
            .annotations
            .get(bookmark_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_annotations(
        &mut self,
        bookmark_id: &str,
        annotations: Vec<Annotation>,
    ) {
        if annotations.is_empty() {
            self.state.annotations.remove(bookmark_id);
        } else {
            self.state
                .annotations
                .insert(bookmark_id.to_string(), annotations);
        }
    }

    pub fn bookmark_ids(&self) -> Vec<String> {
        self.state.bookmarks.keys().cloned().collect()
    }

    pub fn last_sync(&self) -> Option<DateTime<Utc>> {
        self.state.last_sync
    }

    pub fn set_last_sync(&mut self, at: DateTime<Utc>) {
        self.state.last_sync = Some(at);
    }

    /// Drop every cached bookmark, and its article, that is no longer on the
    /// server.
    pub fn retain_bookmarks<F>(&mut self, keep: F)
    where
        F: Fn(&str) -> bool,
    {
        let stale: Vec<String> = self
            .state
            .bookmarks
            .keys()
            .filter(|id| !keep(id))
            .cloned()
            .collect();
        for id in stale {
            self.remove_bookmark(&id);
        }
    }

    pub fn apply_patch(
        &mut self,
        id: &str,
        patch: &crate::models::BookmarkPatch,
    ) {
        if let Some(bookmark) = self.state.bookmarks.get_mut(id) {
            bookmark.apply_patch(patch);
        }
    }

    pub fn queue_patch(
        &mut self,
        id: &str,
        patch: &crate::models::BookmarkPatch,
    ) {
        if self.state.pending.deletes.contains(id) {
            return;
        }
        let entry = self
            .state
            .pending
            .updates
            .entry(id.to_string())
            .or_default();
        merge_patch(entry, patch);
    }

    pub fn queue_delete(&mut self, id: &str) {
        self.state.pending.updates.remove(id);
        self.state.pending.deletes.insert(id.to_string());
    }

    pub fn pending_updates(
        &self,
    ) -> HashMap<String, crate::models::BookmarkPatch> {
        self.state.pending.updates.clone()
    }

    pub fn pending_deletes(&self) -> Vec<String> {
        self.state.pending.deletes.iter().cloned().collect()
    }

    pub fn remove_pending_update(&mut self, id: &str) {
        self.state.pending.updates.remove(id);
    }

    pub fn remove_pending_delete(&mut self, id: &str) {
        self.state.pending.deletes.remove(id);
    }

    pub fn pending_count(&self) -> usize {
        self.state.pending.updates.len() + self.state.pending.deletes.len()
    }

    pub fn remove_bookmark(&mut self, id: &str) {
        self.state.bookmarks.remove(id);
        self.state.annotations.remove(id);
        self.state.pending.updates.remove(id);
        self.state.pending.deletes.remove(id);
        let _ = self.remove_article(id);
    }

    fn article_path(&self, id: &str) -> PathBuf {
        let safe: String = id
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
            .collect();
        self.dir.join("articles").join(format!("{safe}.html"))
    }

    pub fn article_path_for(dir: &Path, id: &str) -> PathBuf {
        let safe: String = id
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
            .collect();
        dir.join("articles").join(format!("{safe}.html"))
    }

    pub fn read_article(&self, id: &str) -> Option<String> {
        fs::read_to_string(self.article_path(id)).ok()
    }

    pub fn has_article(&self, id: &str) -> bool {
        self.article_path(id).exists()
    }

    #[cfg(test)]
    pub fn write_article(&self, id: &str, html: &str) -> Result<()> {
        Self::write_article_to_dir(&self.dir, id, html)
    }

    pub fn write_article_to_dir(
        dir: &Path,
        id: &str,
        html: &str,
    ) -> Result<()> {
        let path = Self::article_path_for(dir, id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("could not create cache dir {}", parent.display())
            })?;
        }
        let temporary = path.with_extension("html.tmp");
        fs::write(&temporary, html).with_context(|| {
            format!("could not write {}", temporary.display())
        })?;
        fs::rename(&temporary, &path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    }

    pub fn remove_article(&self, id: &str) -> Result<()> {
        let path = self.article_path(id);
        if path.exists() {
            fs::remove_file(&path).with_context(|| {
                format!("could not remove {}", path.display())
            })?;
        }
        Ok(())
    }

    /// Return cached bookmarks that match the current view locally.
    ///
    /// Collections are server-side search definitions and cannot be resolved
    /// from bookmark metadata alone, so they produce an empty local view.
    pub fn local_bookmarks(
        &self,
        filter: &Filter,
        search: &SearchQuery,
        sort: Sort,
    ) -> Vec<Bookmark> {
        if matches!(filter, Filter::Collection { .. }) {
            return Vec::new();
        }

        let mut bookmarks: Vec<Bookmark> = self
            .state
            .bookmarks
            .values()
            .filter(|bookmark| !bookmark.is_deleted && filter.matches(bookmark))
            .filter(|bookmark| search.matches(bookmark))
            .cloned()
            .collect();

        bookmarks.sort_by(|a, b| match sort {
            Sort::Newest => b.created.cmp(&a.created),
            Sort::Oldest => a.created.cmp(&b.created),
            Sort::Title => a
                .display_title()
                .to_lowercase()
                .cmp(&b.display_title().to_lowercase()),
            Sort::Site => a.host().to_lowercase().cmp(&b.host().to_lowercase()),
            Sort::Published => {
                b.published_or_created().cmp(&a.published_or_created())
            }
            Sort::ReadingTime => b.reading_time.cmp(&a.reading_time),
            Sort::ReadingTimeShortest => a.reading_time.cmp(&b.reading_time),
        });
        bookmarks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_cache() -> Cache {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("eckly-cache-test-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Cache::new(dir)
    }

    #[test]
    fn merges_and_clears_pending_changes() {
        let mut cache = temp_cache();
        cache.queue_patch(
            "one",
            &crate::models::BookmarkPatch {
                is_marked: Some(true),
                ..Default::default()
            },
        );
        cache.queue_patch(
            "one",
            &crate::models::BookmarkPatch {
                note: Some("offline note".into()),
                ..Default::default()
            },
        );
        let pending = cache.pending_updates();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending["one"].is_marked, Some(true));
        assert_eq!(pending["one"].note.as_deref(), Some("offline note"));

        cache.queue_delete("one");
        assert!(cache.pending_updates().is_empty());
        assert_eq!(cache.pending_deletes(), vec!["one".to_string()]);
        assert_eq!(cache.pending_count(), 1);
    }

    #[test]
    fn caches_metadata_and_articles() {
        let mut cache = temp_cache();
        let bookmark = Bookmark {
            id: "one".into(),
            title: Some("Rust notes".into()),
            labels: vec!["rust".into()],
            created: Some(chrono::Utc::now()),
            ..Default::default()
        };
        cache.upsert_bookmarks(&[bookmark]);
        cache.write_article("one", "<p>cached</p>").unwrap();
        cache.save().unwrap();

        let reloaded = Cache::load(cache.dir.clone()).unwrap();
        assert_eq!(
            reloaded.read_article("one").as_deref(),
            Some("<p>cached</p>")
        );
        assert_eq!(
            reloaded
                .local_bookmarks(
                    &Filter::All,
                    &SearchQuery::parse("rust"),
                    Sort::Newest
                )
                .len(),
            1
        );
        assert_eq!(
            reloaded
                .local_bookmarks(
                    &Filter::Label("rust".into()),
                    &SearchQuery::default(),
                    Sort::Title,
                )
                .len(),
            1
        );
    }

    #[test]
    fn renames_labels_and_merges_counts() {
        let mut cache = temp_cache();
        cache.set_labels(vec![
            Label {
                name: "old".into(),
                count: 2,
            },
            Label {
                name: "keep".into(),
                count: 3,
            },
        ]);
        cache.upsert_bookmarks(&[Bookmark {
            id: "one".into(),
            labels: vec!["old".into(), "keep".into()],
            ..Default::default()
        }]);

        cache.rename_label("old", "keep");

        let labels = cache.labels();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].name, "keep");
        assert_eq!(labels[0].count, 5);

        let bookmarks = cache.local_bookmarks(
            &Filter::All,
            &SearchQuery::default(),
            Sort::Newest,
        );
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].labels, vec!["keep".to_string()]);
    }

    #[test]
    fn caches_annotations_per_bookmark() {
        let mut cache = temp_cache();
        cache.upsert_bookmarks(&[Bookmark {
            id: "one".into(),
            ..Default::default()
        }]);
        cache.set_annotations(
            "one",
            vec![Annotation {
                id: "a1".into(),
                note: "n".into(),
                ..Default::default()
            }],
        );
        assert_eq!(cache.annotations("one").len(), 1);

        cache.set_annotations("one", Vec::new());
        assert!(cache.annotations("one").is_empty());

        cache.set_annotations(
            "one",
            vec![Annotation {
                id: "a1".into(),
                ..Default::default()
            }],
        );
        cache.remove_bookmark("one");
        assert!(cache.annotations("one").is_empty());
    }

    #[test]
    fn pruning_drops_articles_of_removed_bookmarks() {
        let mut cache = temp_cache();
        let bookmarks: Vec<Bookmark> = ["one", "two"]
            .into_iter()
            .map(|id| Bookmark {
                id: id.into(),
                ..Default::default()
            })
            .collect();
        cache.upsert_bookmarks(&bookmarks);
        cache.write_article("one", "<p>one</p>").unwrap();
        cache.write_article("two", "<p>two</p>").unwrap();

        cache.retain_bookmarks(|id| id == "one");

        assert_eq!(
            cache
                .local_bookmarks(
                    &Filter::All,
                    &SearchQuery::default(),
                    Sort::Newest,
                )
                .len(),
            1
        );
        assert!(cache.has_article("one"));
        assert!(!cache.has_article("two"));
    }
}
