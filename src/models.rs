use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

fn deserialize_opt_datetime<'de, D>(
    deserializer: D,
) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value
        .filter(|s| !s.trim().is_empty())
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&Utc)))
}

/// A bookmark as returned by `GET /api/bookmarks` or `GET /api/bookmarks/{id}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Bookmark {
    #[serde(default)]
    pub id: String,
    #[serde(default, deserialize_with = "deserialize_opt_datetime")]
    pub created: Option<DateTime<Utc>>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub site_name: Option<String>,
    #[serde(default)]
    pub site: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_datetime")]
    pub published: Option<DateTime<Utc>>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub has_article: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub is_deleted: bool,
    #[serde(default)]
    pub is_marked: bool,
    #[serde(default)]
    pub is_archived: bool,
    #[serde(default)]
    pub read_progress: u8,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub word_count: u32,
    #[serde(default)]
    pub reading_time: u32,
    #[serde(default)]
    pub state: u8,
    #[serde(default)]
    pub loaded: bool,
}

impl Bookmark {
    pub fn display_title(&self) -> String {
        if let Some(title) = self
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            return title.to_string();
        }
        if !self.url.is_empty() {
            return self.url.clone();
        }
        "(untitled)".to_string()
    }

    pub fn host(&self) -> String {
        self.site_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.site
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
            .map(str::to_string)
            .or_else(|| {
                url::Url::parse(&self.url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_string))
            })
            .unwrap_or_default()
    }

    pub fn status(&self) -> ReadStatus {
        ReadStatus::from_progress(self.read_progress)
    }

    pub fn is_read(&self) -> bool {
        self.read_progress >= 100
    }

    pub fn is_loading(&self) -> bool {
        self.state == 2 || (!self.loaded && self.state != 1)
    }

    pub fn has_error(&self) -> bool {
        self.state == 1
    }

    pub fn published_or_created(&self) -> Option<DateTime<Utc>> {
        self.published.or(self.created)
    }

    pub fn author_line(&self) -> String {
        if self.authors.is_empty() {
            String::new()
        } else if self.authors.len() <= 2 {
            self.authors.join(", ")
        } else {
            format!("{} et al.", self.authors[0])
        }
    }

    pub fn apply_patch(&mut self, patch: &BookmarkPatch) {
        if let Some(v) = &patch.title {
            self.title = Some(v.clone());
        }
        if let Some(v) = &patch.description {
            self.description = Some(v.clone());
        }
        if let Some(v) = &patch.note {
            self.note = Some(v.clone());
        }
        if let Some(v) = patch.is_marked {
            self.is_marked = v;
        }
        if let Some(v) = patch.is_archived {
            self.is_archived = v;
        }
        if let Some(v) = patch.is_deleted {
            self.is_deleted = v;
        }
        if let Some(v) = patch.read_progress {
            self.read_progress = v;
        }
        if let Some(v) = &patch.labels {
            self.labels = v.clone();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadStatus {
    Unread,
    Reading,
    Read,
}

impl ReadStatus {
    pub fn from_progress(progress: u8) -> Self {
        match progress {
            0 => Self::Unread,
            100..=u8::MAX => Self::Read,
            _ => Self::Reading,
        }
    }
}

/// Partial update sent to `PATCH /api/bookmarks/{id}`.
///
/// The API treats every field as optional. Fields set to `None` are omitted
/// from the JSON payload entirely.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BookmarkPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_marked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_archived: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_deleted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_progress: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add_labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove_labels: Option<Vec<String>>,
}

impl BookmarkPatch {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.note.is_none()
            && self.is_marked.is_none()
            && self.is_archived.is_none()
            && self.is_deleted.is_none()
            && self.read_progress.is_none()
            && self.labels.is_none()
            && self.add_labels.is_none()
            && self.remove_labels.is_none()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Label {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub count: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Collection {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_pinned: bool,
    #[serde(default)]
    pub is_deleted: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub user: User,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct User {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub email: String,
}

/// A page of results plus the total count Readeck reports.
#[derive(Debug, Clone, Default)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: usize,
}

/// Which bookmarks to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    All,
    Unread,
    Reading,
    Marked,
    Archived,
    Label(String),
    Collection { id: String, name: String },
}

fn quote_label(name: &str) -> String {
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

impl Filter {
    pub fn title(&self) -> String {
        match self {
            Self::All => "All Bookmarks".into(),
            Self::Unread => "Unread".into(),
            Self::Reading => "Reading".into(),
            Self::Marked => "Favorites".into(),
            Self::Archived => "Archived".into(),
            Self::Label(name) => format!("#{name}"),
            Self::Collection { name, .. } => name.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Newest,
    Oldest,
    Title,
    Site,
    Published,
    ReadingTime,
    ReadingTimeShortest,
}

impl Sort {
    pub const ALL: [Sort; 7] = [
        Self::Newest,
        Self::Oldest,
        Self::Title,
        Self::Site,
        Self::Published,
        Self::ReadingTime,
        Self::ReadingTimeShortest,
    ];

    pub fn api_value(self) -> &'static str {
        match self {
            Self::Newest => "-created",
            Self::Oldest => "created",
            Self::Title => "title",
            Self::Site => "site",
            Self::Published => "-published",
            Self::ReadingTime => "-duration",
            Self::ReadingTimeShortest => "duration",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Newest => "Newest first",
            Self::Oldest => "Oldest first",
            Self::Title => "Title",
            Self::Site => "Site",
            Self::Published => "Published",
            Self::ReadingTime => "Longest reading time",
            Self::ReadingTimeShortest => "Shortest reading time",
        }
    }

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let idx = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Query parameters for the bookmark list endpoint.
#[derive(Debug, Clone)]
pub struct BookmarkQuery {
    pub limit: usize,
    pub offset: usize,
    pub sort: String,
    pub search: Option<String>,
    pub labels: Option<String>,
    pub read_status: Vec<String>,
    pub is_marked: Option<bool>,
    pub is_archived: Option<bool>,
    pub collection: Option<String>,
}

impl BookmarkQuery {
    pub fn new(
        filter: &Filter,
        search: &str,
        sort: Sort,
        limit: usize,
        offset: usize,
    ) -> Self {
        let mut query = Self {
            limit,
            offset,
            sort: sort.api_value().to_string(),
            search: None,
            labels: None,
            read_status: Vec::new(),
            is_marked: None,
            is_archived: None,
            collection: None,
        };

        match filter {
            Filter::All => {}
            Filter::Unread => {
                // The Unread view is an inbox: it includes bookmarks you have
                // finished unless they were archived.
                query.is_archived = Some(false);
            }
            Filter::Reading => {
                query.read_status.push("reading".into());
                query.is_archived = Some(false);
            }
            Filter::Marked => {
                query.is_marked = Some(true);
                query.is_archived = Some(false);
            }
            Filter::Archived => query.is_archived = Some(true),
            Filter::Label(name) => {
                // A label is a complete view of all bookmarks carrying it,
                // including read and archived bookmarks.
                query.labels = Some(quote_label(name));
            }
            Filter::Collection { id, .. } => {
                query.collection = Some(id.clone())
            }
        }

        let search = search.trim();
        if !search.is_empty() {
            query.search = Some(search.to_string());
        }
        query
    }

    pub fn params(&self) -> Vec<(String, String)> {
        let mut params = vec![
            ("limit".into(), self.limit.to_string()),
            ("offset".into(), self.offset.to_string()),
            ("sort".into(), self.sort.clone()),
        ];
        if let Some(v) = &self.search {
            params.push(("search".into(), v.clone()));
        }
        if let Some(v) = &self.labels {
            params.push(("labels".into(), v.clone()));
        }
        if let Some(v) = self.is_marked {
            params.push(("is_marked".into(), v.to_string()));
        }
        if let Some(v) = self.is_archived {
            params.push(("is_archived".into(), v.to_string()));
        }
        if let Some(v) = &self.collection {
            params.push(("collection".into(), v.clone()));
        }
        for status in &self.read_status {
            params.push(("read_status".into(), status.clone()));
        }
        params
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOKMARK_JSON: &str = r#"{
        "id": "abc123",
        "href": "https://example.test/api/bookmarks/abc123",
        "created": "2026-01-02T03:04:05.123456789Z",
        "updated": "2026-02-03T04:05:06Z",
        "state": 0,
        "loaded": true,
        "url": "https://example.com/article",
        "title": "A good article",
        "site_name": "Example",
        "site": "example.com",
        "published": null,
        "authors": ["Ada"],
        "lang": "en",
        "document_type": "article",
        "type": "article",
        "has_article": true,
        "description": "Desc",
        "note": "",
        "is_deleted": false,
        "is_marked": true,
        "is_archived": false,
        "read_progress": 42,
        "labels": ["rust", "tui"],
        "word_count": 1200,
        "reading_time": 6,
        "resources": {"log": {"src": "x"}}
    }"#;

    #[test]
    fn deserializes_bookmark() {
        let bookmark: Bookmark = serde_json::from_str(BOOKMARK_JSON).unwrap();
        assert_eq!(bookmark.id, "abc123");
        assert_eq!(bookmark.display_title(), "A good article");
        assert_eq!(bookmark.host(), "Example");
        assert_eq!(bookmark.status(), ReadStatus::Reading);
        assert!(bookmark.is_marked);
        assert!(bookmark.has_article);
        assert_eq!(bookmark.reading_time, 6);
        assert_eq!(bookmark.labels, vec!["rust", "tui"]);
    }

    #[test]
    fn handles_smaller_responses_and_odd_dates() {
        let bookmark: Bookmark = serde_json::from_str(
            r#"{"id":"x","url":"https://example.com","published":"","created":null}"#,
        )
        .unwrap();
        assert_eq!(bookmark.display_title(), "https://example.com");
        assert_eq!(bookmark.status(), ReadStatus::Unread);
        assert_eq!(bookmark.published, None);
    }

    #[test]
    fn builds_queries() {
        let q = BookmarkQuery::new(
            &Filter::Unread,
            "rust tui",
            Sort::Newest,
            50,
            100,
        );
        let params = q.params();
        assert!(params.contains(&("limit".into(), "50".into())));
        assert!(params.contains(&("offset".into(), "100".into())));
        assert!(params.contains(&("sort".into(), "-created".into())));
        assert!(params.contains(&("search".into(), "rust tui".into())));
        assert!(!params.iter().any(|(key, _)| key == "read_status"));
        assert!(params.contains(&("is_archived".into(), "false".into())));

        let q = BookmarkQuery::new(
            &Filter::Collection {
                id: "c1".into(),
                name: "Later".into(),
            },
            "",
            Sort::Title,
            25,
            0,
        );
        assert!(q.params().contains(&("collection".into(), "c1".into())));
        assert!(!q.params().iter().any(|(k, _)| k == "search"));

        let q = BookmarkQuery::new(
            &Filter::Label("read later".into()),
            "",
            Sort::Newest,
            50,
            0,
        );
        let params = q.params();
        assert!(params.contains(&("labels".into(), "\"read later\"".into())));
        assert!(!params.iter().any(|(key, _)| key == "is_archived"));
        assert!(!params.iter().any(|(key, _)| key == "read_status"));
    }

    #[test]
    fn patch_omits_none_fields() {
        let patch = BookmarkPatch {
            is_marked: Some(true),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&patch).unwrap(),
            r#"{"is_marked":true}"#
        );
    }

    #[test]
    fn sorts_cycle() {
        assert_eq!(Sort::Newest.next(), Sort::Oldest);
        assert_eq!(Sort::ReadingTime.next(), Sort::ReadingTimeShortest);
        assert_eq!(Sort::ReadingTimeShortest.next(), Sort::Newest);
        assert_eq!(Sort::Newest.previous(), Sort::ReadingTimeShortest);
        assert_eq!(Sort::ReadingTimeShortest.previous(), Sort::ReadingTime);
        assert_eq!(Sort::Oldest.previous(), Sort::Newest);
    }
}
