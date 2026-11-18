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

fn decode_entities(value: &str) -> String {
    html_escape::decode_html_entities(value).into_owned()
}

fn deserialize_opt_entity_string<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value.map(|value| decode_entities(&value)))
}

fn deserialize_entity_string<'de, D>(
    deserializer: D,
) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    Ok(decode_entities(&value))
}

fn deserialize_entity_list<'de, D>(
    deserializer: D,
) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<String>::deserialize(deserializer)?;
    Ok(values.iter().map(|value| decode_entities(value)).collect())
}

/// A bookmark as returned by `GET /api/bookmarks` or `GET /api/bookmarks/{id}`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Bookmark {
    #[serde(default)]
    pub id: String,
    #[serde(default, deserialize_with = "deserialize_opt_datetime")]
    pub created: Option<DateTime<Utc>>,
    #[serde(default)]
    pub url: String,
    #[serde(default, deserialize_with = "deserialize_opt_entity_string")]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_entity_string")]
    pub site_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_entity_string")]
    pub site: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_datetime")]
    pub published: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "deserialize_entity_list")]
    pub authors: Vec<String>,
    #[serde(default)]
    pub has_article: bool,
    #[serde(default, deserialize_with = "deserialize_opt_entity_string")]
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
    #[serde(default, deserialize_with = "deserialize_entity_list")]
    pub labels: Vec<String>,
    #[serde(default)]
    pub word_count: u32,
    #[serde(default)]
    pub reading_time: u32,
    #[serde(default)]
    pub state: u8,
    #[serde(default)]
    pub loaded: bool,
    #[serde(default)]
    pub lang: String,
    #[serde(default)]
    pub document_type: String,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Label {
    #[serde(default, deserialize_with = "deserialize_entity_string")]
    pub name: String,
    #[serde(default)]
    pub count: usize,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Collection {
    #[serde(default)]
    pub id: String,
    #[serde(default, deserialize_with = "deserialize_entity_string")]
    pub name: String,
    #[serde(default)]
    pub is_pinned: bool,
    #[serde(default)]
    pub is_deleted: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Profile {
    #[serde(default)]
    pub user: User,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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

/// One entry from `GET /api/bookmarks/sync`, the server's change log.
#[derive(Debug, Clone, Deserialize)]
pub struct SyncChange {
    pub id: String,
    #[serde(deserialize_with = "deserialize_datetime")]
    pub time: DateTime<Utc>,
    #[serde(rename = "type")]
    pub kind: SyncChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncChangeKind {
    Update,
    Delete,
}

fn deserialize_datetime<'de, D>(
    deserializer: D,
) -> Result<DateTime<Utc>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    DateTime::parse_from_rfc3339(&value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(serde::de::Error::custom)
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
    /// Local membership test used for cached bookmarks and post-update
    /// pruning. Collection membership is only known by the server.
    pub fn matches(&self, bookmark: &Bookmark) -> bool {
        match self {
            Self::All => true,
            Self::Unread => !bookmark.is_archived,
            Self::Reading => {
                (1..100).contains(&bookmark.read_progress)
                    && !bookmark.is_archived
            }
            Self::Marked => bookmark.is_marked && !bookmark.is_archived,
            Self::Archived => bookmark.is_archived,
            Self::Label(name) => {
                bookmark.labels.iter().any(|label| label == name)
            }
            Self::Collection { .. } => true,
        }
    }

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

/// Filters parsed from the search box.
///
/// Free text and unrecognized `field:value` terms stay in [`Self::text`] and
/// are sent as the API `search` parameter, so Readeck's own query syntax
/// (`title:`, `author:`, `site:`, `note:`, quotes, wildcards and
/// negation) keeps working.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub types: Vec<String>,
    pub labels: Vec<String>,
    pub lang: Option<String>,
    pub read_status: Vec<String>,
    pub is_marked: Option<bool>,
    pub is_archived: Option<bool>,
    pub is_loaded: Option<bool>,
    pub has_errors: Option<bool>,
    pub has_labels: Option<bool>,
    pub has_notes: Option<bool>,
    pub range_start: Option<String>,
    pub range_end: Option<String>,
    pub ids: Vec<String>,
    /// Set when a filter value is not supported.
    pub error: Option<String>,
}

impl SearchQuery {
    pub fn parse(input: &str) -> Self {
        let mut query = Self::default();
        let mut text = Vec::new();
        for token in tokenize(input) {
            let Some((key, value)) = token.split_once(':') else {
                text.push(token);
                continue;
            };
            let key = key.to_ascii_lowercase();
            let value = unquote_value(value);
            if value.is_empty() {
                text.push(token);
                continue;
            }
            let lower = value.to_ascii_lowercase();
            match key.as_str() {
                "type" => match lower.as_str() {
                    "article" => query.push_type("article"),
                    "photo" | "picture" => query.push_type("photo"),
                    "video" => query.push_type("video"),
                    _ => query.fail("type", &lower, "article, photo or video"),
                },
                "lang" => query.lang = Some(value.to_string()),
                "label" | "labels" => query.push_label(value),
                "status" => match lower.as_str() {
                    "unread" => query.push_status("unread"),
                    "reading" => query.push_status("reading"),
                    "read" => query.push_status("read"),
                    _ => {
                        query.fail("status", &lower, "unread, reading or read")
                    }
                },
                "is" => match lower.as_str() {
                    "favorite" | "marked" => query.is_marked = Some(true),
                    "archived" => query.is_archived = Some(true),
                    "loaded" => query.is_loaded = Some(true),
                    _ => {
                        query.fail("is", &lower, "favorite, archived or loaded")
                    }
                },
                "has" => match lower.as_str() {
                    "labels" => query.has_labels = Some(true),
                    "notes" => query.has_notes = Some(true),
                    "errors" => query.has_errors = Some(true),
                    _ => query.fail("has", &lower, "labels, notes or errors"),
                },
                "after" => query.range_start = Some(value.to_string()),
                "before" => query.range_end = Some(value.to_string()),
                "id" => {
                    if !query.ids.iter().any(|id| id == value) {
                        query.ids.push(value.to_string());
                    }
                }
                // Field terms and unknown prefixes remain full-text terms.
                _ => text.push(token),
            }
            if query.error.is_some() {
                return query;
            }
        }
        query.text = text.join(" ");
        query
    }

    fn push_type(&mut self, kind: &str) {
        if !self.types.iter().any(|existing| existing == kind) {
            self.types.push(kind.to_string());
        }
    }

    fn push_label(&mut self, label: &str) {
        if !self
            .labels
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(label))
        {
            self.labels.push(label.to_string());
        }
    }

    fn push_status(&mut self, status: &str) {
        if !self.read_status.iter().any(|existing| existing == status) {
            self.read_status.push(status.to_string());
        }
    }

    fn fail(&mut self, key: &str, value: &str, choices: &str) {
        if self.error.is_none() {
            self.error =
                Some(format!("Invalid {key}: “{value}” (use {choices})"));
        }
    }

    /// Local membership test for the cached list.
    pub fn matches(&self, bookmark: &Bookmark) -> bool {
        if !self.text.is_empty() && !local_text_matches(bookmark, &self.text) {
            return false;
        }
        if !self.types.is_empty()
            && !self
                .types
                .iter()
                .any(|kind| kind.eq_ignore_ascii_case(&bookmark.document_type))
        {
            return false;
        }
        if let Some(lang) = &self.lang {
            if !wildcard_eq(lang, &bookmark.lang) {
                return false;
            }
        }
        if !self.labels.is_empty()
            && !self.labels.iter().all(|label| {
                bookmark
                    .labels
                    .iter()
                    .any(|value| value.eq_ignore_ascii_case(label))
            })
        {
            return false;
        }
        if !self.read_status.is_empty() {
            let status = match bookmark.status() {
                ReadStatus::Unread => "unread",
                ReadStatus::Reading => "reading",
                ReadStatus::Read => "read",
            };
            if !self.read_status.iter().any(|value| value == status) {
                return false;
            }
        }
        if self.is_marked == Some(true) && !bookmark.is_marked {
            return false;
        }
        if self.is_archived == Some(true) && !bookmark.is_archived {
            return false;
        }
        if self.is_loaded == Some(true) && !bookmark.loaded {
            return false;
        }
        if self.has_errors == Some(true) && !bookmark.has_error() {
            return false;
        }
        if self.has_labels == Some(true) && bookmark.labels.is_empty() {
            return false;
        }
        if self.has_notes == Some(true)
            && bookmark
                .note
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
        {
            return false;
        }
        if !self.ids.is_empty() && !self.ids.iter().any(|id| id == &bookmark.id)
        {
            return false;
        }
        if let Some(start) = self.range_start.as_deref().and_then(parse_time) {
            if bookmark.created.is_none_or(|created| created < start) {
                return false;
            }
        }
        if let Some(end) = self.range_end.as_deref().and_then(parse_time) {
            if bookmark.created.is_none_or(|created| created > end) {
                return false;
            }
        }
        true
    }
}

/// Split a query into tokens, keeping quoted strings together.
fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for ch in input.chars() {
        match quote {
            Some(delim) => {
                current.push(ch);
                if ch == delim {
                    quote = None;
                }
            }
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => {
                current.push(ch);
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                }
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn unquote_value(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
}

/// Match a value against a pattern where `*` matches anything.
fn wildcard_eq(pattern: &str, value: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let value = value.to_ascii_lowercase();
    if !pattern.contains('*') {
        return value == pattern;
    }
    let mut remainder = value.as_str();
    let mut first = true;
    for part in pattern.split('*') {
        if part.is_empty() {
            first = false;
            continue;
        }
        if first {
            let Some(rest) = remainder.strip_prefix(part) else {
                return false;
            };
            remainder = rest;
        } else {
            let Some(index) = remainder.find(part) else {
                return false;
            };
            remainder = &remainder[index + part.len()..];
        }
        first = false;
    }
    if pattern.ends_with('*') {
        true
    } else {
        remainder.is_empty()
    }
}

/// Parse a Readeck time token for local date filtering.
///
/// Supports `now`, `±<n><d|w|m|y>`, RFC 3339 and `YYYY-MM-DD`.
fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim().to_ascii_lowercase();
    if value == "now" {
        return Some(Utc::now());
    }
    let (sign, rest) = if let Some(rest) = value.strip_prefix('-') {
        (-1i64, rest)
    } else if let Some(rest) = value.strip_prefix('+') {
        (1i64, rest)
    } else {
        (1i64, value.as_str())
    };
    let mut chars = rest.chars();
    if let Some(unit) = chars.next_back() {
        if let Ok(count) = chars.as_str().parse::<i64>() {
            let count = count * sign;
            let now = Utc::now();
            match unit {
                'd' => return Some(now + chrono::Duration::days(count)),
                'w' => return Some(now + chrono::Duration::weeks(count)),
                'm' => {
                    return if count >= 0 {
                        now.checked_add_months(chrono::Months::new(
                            count as u32,
                        ))
                    } else {
                        now.checked_sub_months(chrono::Months::new(
                            (-count) as u32,
                        ))
                    };
                }
                'y' => {
                    let months = count.unsigned_abs() as u32 * 12;
                    return if count >= 0 {
                        now.checked_add_months(chrono::Months::new(months))
                    } else {
                        now.checked_sub_months(chrono::Months::new(months))
                    };
                }
                _ => {}
            }
        }
    }
    DateTime::parse_from_rfc3339(&value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(&value, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|date| date.and_utc())
        })
}

fn local_text_matches(bookmark: &Bookmark, query: &str) -> bool {
    let query = query.to_ascii_lowercase();
    let mut fields = Vec::new();
    if let Some(title) = &bookmark.title {
        fields.push(title.as_str());
    }
    if let Some(description) = &bookmark.description {
        fields.push(description.as_str());
    }
    if let Some(note) = &bookmark.note {
        fields.push(note.as_str());
    }
    if let Some(site_name) = &bookmark.site_name {
        fields.push(site_name.as_str());
    }
    if let Some(site) = &bookmark.site {
        fields.push(site.as_str());
    }
    fields.push(bookmark.url.as_str());
    fields.extend(bookmark.authors.iter().map(String::as_str));
    fields.extend(bookmark.labels.iter().map(String::as_str));
    fields
        .iter()
        .any(|field| field.to_ascii_lowercase().contains(&query))
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
    pub types: Vec<String>,
    pub lang: Option<String>,
    pub is_loaded: Option<bool>,
    pub has_errors: Option<bool>,
    pub has_labels: Option<bool>,
    pub has_notes: Option<bool>,
    pub range_start: Option<String>,
    pub range_end: Option<String>,
    pub ids: Vec<String>,
}

impl BookmarkQuery {
    pub fn new(
        filter: &Filter,
        search: &SearchQuery,
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
            types: Vec::new(),
            lang: None,
            is_loaded: None,
            has_errors: None,
            has_labels: None,
            has_notes: None,
            range_start: None,
            range_end: None,
            ids: Vec::new(),
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

        let search = search.clone();
        if !search.text.is_empty() {
            query.search = Some(search.text);
        }
        if !search.labels.is_empty() {
            let labels = search
                .labels
                .iter()
                .map(|label| quote_label(label))
                .collect::<Vec<_>>()
                .join(" ");
            query.labels = Some(match query.labels.take() {
                Some(view) => format!("{view} {labels}"),
                None => labels,
            });
        }
        // Search filters override the view they are combined with.
        if !search.types.is_empty() {
            query.types = search.types;
        }
        if search.lang.is_some() {
            query.lang = search.lang;
        }
        if !search.read_status.is_empty() {
            query.read_status = search.read_status;
        }
        if let Some(value) = search.is_marked {
            query.is_marked = Some(value);
        }
        if let Some(value) = search.is_archived {
            query.is_archived = Some(value);
        }
        if let Some(value) = search.is_loaded {
            query.is_loaded = Some(value);
        }
        if let Some(value) = search.has_errors {
            query.has_errors = Some(value);
        }
        if let Some(value) = search.has_labels {
            query.has_labels = Some(value);
        }
        if let Some(value) = search.has_notes {
            query.has_notes = Some(value);
        }
        if search.range_start.is_some() {
            query.range_start = search.range_start;
        }
        if search.range_end.is_some() {
            query.range_end = search.range_end;
        }
        if !search.ids.is_empty() {
            query.ids = search.ids;
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
        for kind in &self.types {
            params.push(("type".into(), kind.clone()));
        }
        if let Some(v) = &self.lang {
            params.push(("lang".into(), v.clone()));
        }
        for (name, value) in [
            ("is_loaded", self.is_loaded),
            ("has_errors", self.has_errors),
            ("has_labels", self.has_labels),
            ("has_notes", self.has_notes),
        ] {
            if let Some(value) = value {
                params.push((name.into(), value.to_string()));
            }
        }
        if let Some(v) = &self.range_start {
            params.push(("range_start".into(), v.clone()));
        }
        if let Some(v) = &self.range_end {
            params.push(("range_end".into(), v.clone()));
        }
        for id in &self.ids {
            params.push(("id".into(), id.clone()));
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
    fn decodes_html_entities_in_metadata() {
        let bookmark: Bookmark = serde_json::from_str(
            r#"{
                "id":"x",
                "url":"https://example.com",
                "title":"&#34;Ziemlich dumm&#34;: Pistorius",
                "description":"A &amp; B",
                "site_name":"&quot;Example&quot;",
                "site":"example.com",
                "authors":["M&#252;ller","Smith &amp; Wesson"],
                "labels":["a&amp;b","caf&#233;"]
            }"#,
        )
        .unwrap();
        assert_eq!(bookmark.display_title(), "\"Ziemlich dumm\": Pistorius");
        assert_eq!(bookmark.description.as_deref(), Some("A & B"));
        assert_eq!(bookmark.site_name.as_deref(), Some("\"Example\""));
        assert_eq!(bookmark.host(), "\"Example\"");
        assert_eq!(bookmark.authors, vec!["Müller", "Smith & Wesson"]);
        assert_eq!(bookmark.labels, vec!["a&b", "café"]);

        let label: Label =
            serde_json::from_str(r#"{"name":"caf&#233;","count":2}"#).unwrap();
        assert_eq!(label.name, "café");
        let collection: Collection =
            serde_json::from_str(r#"{"id":"c","name":"A &amp; B"}"#).unwrap();
        assert_eq!(collection.name, "A & B");
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
            &SearchQuery::parse("rust tui"),
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
            &SearchQuery::default(),
            Sort::Title,
            25,
            0,
        );
        assert!(q.params().contains(&("collection".into(), "c1".into())));
        assert!(!q.params().iter().any(|(k, _)| k == "search"));

        let q = BookmarkQuery::new(
            &Filter::Label("read later".into()),
            &SearchQuery::default(),
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
    fn parses_search_filters() {
        let query = SearchQuery::parse(
            "rust type:VIDEO lang:de status:read is:favorite has:labels \
             after:-7d before:2026-01-01 id:abc",
        );
        assert_eq!(query.text, "rust");
        assert_eq!(query.types, vec!["video"]);
        assert_eq!(query.lang.as_deref(), Some("de"));
        assert_eq!(query.read_status, vec!["read"]);
        assert_eq!(query.is_marked, Some(true));
        assert_eq!(query.has_labels, Some(true));
        assert_eq!(query.range_start.as_deref(), Some("-7d"));
        assert_eq!(query.range_end.as_deref(), Some("2026-01-01"));
        assert_eq!(query.ids, vec!["abc"]);
        assert!(query.error.is_none());

        let query = SearchQuery::parse(
            "title:rust https://example.com/foo label:\"read later\" -cat",
        );
        assert_eq!(query.text, "title:rust https://example.com/foo -cat");
        assert_eq!(query.labels, vec!["read later"]);
        assert!(query.types.is_empty());
        assert!(query.ids.is_empty());
    }

    #[test]
    fn parses_quoted_label_filters() {
        let query =
            SearchQuery::parse("label:\"read later\" label:rust labels:tui");
        assert_eq!(query.text, "");
        assert_eq!(query.labels, vec!["read later", "rust", "tui"]);
        assert!(query.error.is_none());

        let query =
            SearchQuery::parse("title:\"read later\" label:\"read later\"");
        assert_eq!(query.text, "title:\"read later\"");
        assert_eq!(query.labels, vec!["read later"]);
    }

    #[test]
    fn label_filters_combine_with_the_view() {
        let query = BookmarkQuery::new(
            &Filter::Label("view".into()),
            &SearchQuery::parse("label:rust label:\"read later\""),
            Sort::Newest,
            10,
            0,
        );
        assert!(query.params().contains(&(
            "labels".into(),
            "\"view\" \"rust\" \"read later\"".into(),
        )));
    }

    #[test]
    fn rejects_unknown_filter_values() {
        assert!(SearchQuery::parse("type:gif").error.is_some());
        assert!(SearchQuery::parse("status:done").error.is_some());
        assert!(SearchQuery::parse("is:hot").error.is_some());
        assert!(SearchQuery::parse("has:images").error.is_some());
        assert!(
            SearchQuery::parse("type:article status:read")
                .error
                .is_none()
        );
    }

    #[test]
    fn search_filters_override_the_view_and_set_parameters() {
        let query = BookmarkQuery::new(
            &Filter::Unread,
            &SearchQuery::parse(
                "term lang:de has:notes is:archived is:loaded after:-1w \
                 before:2026-01-01 id:a id:b type:photo status:read",
            ),
            Sort::Newest,
            10,
            0,
        );
        let params = query.params();
        assert!(params.contains(&("search".into(), "term".into())));
        assert!(params.contains(&("lang".into(), "de".into())));
        assert!(params.contains(&("has_notes".into(), "true".into())));
        assert!(params.contains(&("is_loaded".into(), "true".into())));
        assert!(params.contains(&("range_start".into(), "-1w".into())));
        assert!(params.contains(&("range_end".into(), "2026-01-01".into())));
        assert!(params.contains(&("id".into(), "a".into())));
        assert!(params.contains(&("id".into(), "b".into())));
        assert!(params.contains(&("type".into(), "photo".into())));
        // The search overrides the Unread view.
        assert!(params.contains(&("is_archived".into(), "true".into())));
        assert!(params.contains(&("read_status".into(), "read".into())));
    }

    #[test]
    fn search_query_matches_cached_bookmarks() {
        let bookmark = Bookmark {
            id: "one".into(),
            document_type: "photo".into(),
            lang: "de".into(),
            is_marked: true,
            is_archived: true,
            loaded: true,
            title: Some("Rust notes".into()),
            labels: vec!["rust".into()],
            note: Some("note".into()),
            created: Some("2026-01-15T00:00:00Z".parse().unwrap()),
            ..Default::default()
        };
        assert!(SearchQuery::parse("type:photo").matches(&bookmark));
        assert!(!SearchQuery::parse("type:video").matches(&bookmark));
        assert!(SearchQuery::parse("lang:de").matches(&bookmark));
        assert!(SearchQuery::parse("lang:d*").matches(&bookmark));
        assert!(!SearchQuery::parse("lang:en").matches(&bookmark));
        assert!(SearchQuery::parse("status:unread").matches(&bookmark));
        assert!(!SearchQuery::parse("status:read").matches(&bookmark));
        assert!(
            SearchQuery::parse("is:favorite is:archived is:loaded")
                .matches(&bookmark)
        );
        assert!(SearchQuery::parse("has:labels has:notes").matches(&bookmark));
        assert!(SearchQuery::parse("RUST").matches(&bookmark));
        assert!(!SearchQuery::parse("has:errors").matches(&bookmark));
        assert!(SearchQuery::parse("label:rust").matches(&bookmark));
        assert!(SearchQuery::parse("label:RUST").matches(&bookmark));
        assert!(!SearchQuery::parse("label:tui").matches(&bookmark));
        assert!(SearchQuery::parse("id:one").matches(&bookmark));
        assert!(!SearchQuery::parse("id:two").matches(&bookmark));
        assert!(
            SearchQuery::parse("after:2026-01-01 before:2026-02-01")
                .matches(&bookmark)
        );
        assert!(!SearchQuery::parse("after:2026-02-01").matches(&bookmark));
        assert!(!SearchQuery::parse("before:2026-01-01").matches(&bookmark));
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
