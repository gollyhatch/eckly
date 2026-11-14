use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use image::DynamicImage;
use ratatui::widgets::ListState;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiError, OAuthDevice, OAuthPoll, ReadeckClient};
use crate::article::{Article, parse_article};
use crate::config::{Config, normalize_server};
use crate::event::{ApiEvent, Event};
use crate::input::TextInput;
use crate::models::{
    Bookmark, BookmarkPatch, BookmarkQuery, Collection, Filter, Label, Profile,
    Sort,
};

const PAGE_SIZE: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Setup,
    Main,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Sidebar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupField {
    Server,
    Token,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub expires: u64,
}

#[derive(Debug, Clone)]
pub struct SetupState {
    pub server: TextInput,
    pub token: TextInput,
    pub focus: SetupField,
    pub error: Option<String>,
    pub busy: bool,
}

impl SetupState {
    pub fn new(config: &Config, error: Option<String>) -> Self {
        let server = TextInput::new(&config.server);
        let mut token = TextInput::masked(&config.token);
        token.cursor = token.char_len();
        let focus = if config.server.trim().is_empty() {
            SetupField::Server
        } else {
            SetupField::Token
        };
        Self {
            server,
            token,
            focus,
            error,
            busy: false,
        }
    }

    fn next_field(&mut self) {
        self.focus = match self.focus {
            SetupField::Server => SetupField::Token,
            SetupField::Token => SetupField::Server,
        };
    }

    fn previous_field(&mut self) {
        self.next_field();
    }

    fn active_input_mut(&mut self) -> &mut TextInput {
        match self.focus {
            SetupField::Server => &mut self.server,
            SetupField::Token => &mut self.token,
        }
    }
}

#[derive(Debug)]
pub struct OAuthState {
    pub server: String,
    pub device: Option<OAuthDevice>,
    pub error: Option<String>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub struct Reader {
    pub bookmark: Bookmark,
    pub article: Option<Article>,
    pub lines: Vec<crate::article::WrappedLine>,
    pub width: usize,
    pub scroll: usize,
    pub viewport: usize,
    pub loading: bool,
    pub error: Option<String>,
    pub initial_progress: u8,
    /// Whether the user moved the reading position in this session.
    pub scrolled: bool,
    /// Set after an explicit read/unread toggle; don't overwrite it on exit.
    pub manual_read_state: bool,
    pub images: HashMap<String, DynamicImage>,
    pub image_errors: HashMap<String, String>,
    pub requested_images: HashSet<String>,
    pub show_images: bool,
    /// Maximum wrapping width; 0 uses the full reader viewport.
    pub max_text_width: usize,
    /// Center the constrained reading column when it is narrower.
    pub center_text: bool,
    /// Active in-article search query.
    pub search_query: Option<String>,
}

impl Reader {
    pub fn new(
        bookmark: Bookmark,
        show_images: bool,
        max_text_width: usize,
        center_text: bool,
    ) -> Self {
        let loading = bookmark.has_article;
        Self {
            initial_progress: bookmark.read_progress,
            scrolled: false,
            manual_read_state: false,
            bookmark,
            article: None,
            lines: Vec::new(),
            width: 0,
            scroll: 0,
            viewport: 20,
            loading,
            error: None,
            images: HashMap::new(),
            image_errors: HashMap::new(),
            requested_images: HashSet::new(),
            show_images,
            max_text_width,
            center_text,
            search_query: None,
        }
    }

    pub fn set_article(&mut self, article: Article) {
        self.article = Some(article);
        self.loading = false;
        self.error = None;
        self.images.clear();
        self.image_errors.clear();
        self.requested_images.clear();
        self.lines.clear();
        self.reflow(self.width.max(60));
        self.restore_progress_position();
    }

    fn restore_progress_position(&mut self) {
        let max = self.max_scroll();
        let ratio = self.initial_progress.min(100) as f32 / 100.0;
        self.scroll = (max as f32 * ratio).round() as usize;
        self.scrolled = false;
    }

    pub fn set_image(&mut self, url: String, image: DynamicImage) {
        self.images.insert(url.clone(), image);
        self.image_errors.remove(&url);
        self.invalidate_layout();
    }

    pub fn set_image_error(&mut self, url: String, error: String) {
        self.image_errors.insert(url, error);
        self.invalidate_layout();
    }

    fn position_ratio(&self) -> f32 {
        let max = self.max_scroll();
        if max > 0 {
            self.scroll as f32 / max as f32
        } else {
            0.0
        }
    }

    fn invalidate_layout(&mut self) {
        let ratio = self.position_ratio();
        let width = self.effective_width(self.width.max(60));
        self.width = width;
        self.lines = self
            .article
            .as_ref()
            .map(|article| {
                if self.show_images
                    && (!self.images.is_empty()
                        || !self.image_errors.is_empty())
                {
                    article.layout_with_images(
                        width,
                        &self.images,
                        &self.image_errors,
                    )
                } else {
                    article.layout(width)
                }
            })
            .unwrap_or_default();
        let max = self.max_scroll();
        self.scroll = ((ratio * max as f32) as usize).min(max);
    }

    fn effective_width(&self, width: usize) -> usize {
        if self.max_text_width == 0 {
            width
        } else {
            width.min(self.max_text_width.max(12))
        }
    }

    pub fn reflow(&mut self, width: usize) {
        let width = self.effective_width(width);
        let Some(article) = &self.article else {
            self.width = width;
            return;
        };
        if width == self.width && !self.lines.is_empty() {
            return;
        }
        let ratio = self.position_ratio();
        self.width = width;
        self.lines = if self.show_images
            && (!self.images.is_empty() || !self.image_errors.is_empty())
        {
            article.layout_with_images(width, &self.images, &self.image_errors)
        } else {
            article.layout(width)
        };
        let max = self.max_scroll();
        self.scroll = ((ratio * max as f32) as usize).min(max);
    }

    pub fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.viewport.max(1))
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.max_scroll() as isize;
        let next = (self.scroll as isize + delta).clamp(0, max) as usize;
        if next != self.scroll {
            self.scroll = next;
            self.scrolled = true;
        }
    }

    /// Jump to a position without treating it as user scrolling.
    pub fn scroll_to(&mut self, position: usize) {
        self.scroll = position.min(self.max_scroll());
    }

    /// Jump to a position and remember that the user moved the reading point.
    pub fn jump_to(&mut self, position: usize) {
        let next = position.min(self.max_scroll());
        if next != self.scroll {
            self.scroll = next;
            self.scrolled = true;
        }
    }

    /// Move to the next block/paragraph start, falling back to a page when
    /// there are no more block boundaries below the current position.
    pub fn scroll_to_next_paragraph(&mut self) {
        let next = self
            .lines
            .iter()
            .enumerate()
            .skip(self.scroll.saturating_add(1))
            .find(|(index, line)| {
                line.kind.is_some()
                    && (*index == 0 || self.lines[*index - 1].kind.is_none())
            })
            .map(|(index, _)| index);

        match next {
            Some(position) => self.jump_to(position),
            None => self.scroll_by(self.page_amount() as isize),
        }
    }

    pub fn page_amount(&self) -> usize {
        self.viewport.saturating_sub(1).max(1)
    }

    pub fn half_page_amount(&self) -> usize {
        (self.viewport / 2).max(1)
    }

    /// Reading position as a percentage, used in the reader footer.
    pub fn progress(&self) -> u8 {
        if self.lines.is_empty() {
            return self.initial_progress;
        }
        let max = self.max_scroll();
        if max == 0 {
            return 100;
        }
        ((self.scroll as f32 / max as f32) * 100.0).round() as u8
    }

    fn line_contains(text: &str, query: &str) -> bool {
        text.to_ascii_lowercase()
            .contains(&query.to_ascii_lowercase())
    }

    pub fn search_match_lines(&self) -> Vec<usize> {
        let Some(query) = self.search_query.as_deref() else {
            return Vec::new();
        };
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, line)| Self::line_contains(&line.text, query))
            .map(|(index, _)| index)
            .collect()
    }

    /// Search the article and jump to the first match at or below the
    /// current reading position. Returns the number of matching lines.
    pub fn search(&mut self, query: &str) -> usize {
        let query = query.trim();
        if query.is_empty() {
            self.search_query = None;
            return 0;
        }
        self.search_query = Some(query.to_string());
        let matches = self.search_match_lines();
        let target = matches
            .iter()
            .copied()
            .find(|&index| index >= self.scroll)
            .or_else(|| matches.first().copied());
        if let Some(index) = target {
            self.jump_to(index);
        }
        matches.len()
    }

    pub fn next_search_match(&mut self, forward: bool) {
        let matches = self.search_match_lines();
        if matches.is_empty() {
            return;
        }
        let target = if forward {
            matches.iter().copied().find(|&index| index > self.scroll)
        } else {
            matches
                .iter()
                .rev()
                .copied()
                .find(|&index| index < self.scroll)
        };
        if let Some(index) = target {
            self.jump_to(index);
        }
    }
}

#[derive(Debug, Clone)]
pub struct SidebarItem {
    pub filter: Option<Filter>,
    pub text: String,
    pub count: Option<usize>,
    pub header: bool,
}

#[derive(Debug)]
pub enum Modal {
    Help,
    Confirm(ConfirmModal),
    Input(InputModal),
    ThemePicker(ThemePicker),
}

#[derive(Debug)]
pub struct ThemePicker {
    pub selected: usize,
    pub original: String,
}

#[derive(Debug)]
pub struct ConfirmModal {
    pub title: String,
    pub message: String,
    pub action: ConfirmAction,
}

#[derive(Debug)]
pub enum ConfirmAction {
    DeleteBookmarks { ids: Vec<String> },
}

#[derive(Debug)]
pub struct InputModal {
    pub title: String,
    pub prompt: String,
    pub input: TextInput,
    pub kind: InputKind,
    pub suggestions: Vec<String>,
    pub completion: Option<LabelCompletion>,
}

impl InputModal {
    pub fn new(
        title: impl Into<String>,
        prompt: impl Into<String>,
        input: TextInput,
        kind: InputKind,
    ) -> Self {
        Self {
            title: title.into(),
            prompt: prompt.into(),
            input,
            kind,
            suggestions: Vec::new(),
            completion: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LabelCompletion {
    pub candidates: Vec<String>,
    pub index: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
pub enum InputKind {
    Search,
    ReaderSearch,
    NewBookmark,
    EditLabels { ids: Vec<String> },
    EditNote { id: String },
    EditTitle { id: String },
}

pub struct App {
    pub screen: Screen,
    pub focus: Focus,
    pub config: Config,
    pub config_path: PathBuf,
    pub client: Option<ReadeckClient>,
    pub setup: Option<SetupState>,
    pub oauth: Option<OAuthState>,
    pub profile: Option<Profile>,
    profile_denied: bool,

    pub bookmarks: Vec<Bookmark>,
    pub selected_ids: HashSet<String>,
    pub list_state: ListState,
    /// Number of two-line bookmark items visible in the list viewport.
    pub list_viewport: usize,
    pub labels: Vec<Label>,
    pub collections: Vec<Collection>,
    pub sidebar_items: Vec<SidebarItem>,
    pub sidebar_state: ListState,
    pub sidebar_viewport: usize,
    pub sidebar_search: TextInput,
    pub sidebar_search_active: bool,
    pub filter: Filter,
    pub search: String,
    pub sort: Sort,
    pub total: usize,
    pub loading_list: bool,
    pub loading_more: bool,
    pub list_error: Option<String>,

    pub reader: Option<Reader>,
    pub modal: Option<Modal>,
    pub toast: Option<Toast>,
    pub tick: u64,
    pub quit: bool,
    pub images_enabled: bool,

    tx: UnboundedSender<Event>,
    list_seq: u64,
    pending_select: Option<String>,
    oauth_seq: u64,
}

impl App {
    pub fn new(
        config: Config,
        config_path: PathBuf,
        tx: UnboundedSender<Event>,
    ) -> Self {
        let images_enabled = config.images;
        Self {
            screen: Screen::Setup,
            focus: Focus::List,
            config,
            config_path,
            client: None,
            setup: None,
            oauth: None,
            profile: None,
            profile_denied: false,
            bookmarks: Vec::new(),
            selected_ids: HashSet::new(),
            list_state: ListState::default(),
            list_viewport: 10,
            labels: Vec::new(),
            collections: Vec::new(),
            sidebar_items: Vec::new(),
            sidebar_state: ListState::default(),
            sidebar_viewport: 10,
            sidebar_search: TextInput::new(""),
            sidebar_search_active: false,
            filter: Filter::Unread,
            search: String::new(),
            sort: Sort::Newest,
            total: 0,
            loading_list: false,
            loading_more: false,
            list_error: None,
            reader: None,
            modal: None,
            toast: None,
            tick: 0,
            quit: false,
            images_enabled,
            tx,
            list_seq: 0,
            pending_select: None,
            oauth_seq: 0,
        }
    }

    /// Connect using saved/environment credentials or present the setup form.
    pub fn bootstrap(&mut self) {
        if !self.config.has_credentials() {
            self.show_setup(None);
            return;
        }

        match ReadeckClient::new(&self.config.server, &self.config.token) {
            Ok(client) => {
                self.client = Some(client);
                self.enter_main();
            }
            Err(err) => self.show_setup(Some(err.to_string())),
        }
    }

    fn enter_main(&mut self) {
        self.screen = Screen::Main;
        self.setup = None;
        self.focus = Focus::List;
        self.request_bookmarks(false, false);
        self.request_labels();
        self.request_collections();
        if self.profile.is_none() && !self.profile_denied {
            self.request_profile();
        }
        self.rebuild_sidebar();
    }

    fn show_setup(&mut self, error: Option<String>) {
        self.screen = Screen::Setup;
        self.client = None;
        if let Some(oauth) = self.oauth.take() {
            oauth.cancel.store(true, Ordering::Relaxed);
        }
        self.oauth_seq = self.oauth_seq.wrapping_add(1);
        self.reader = None;
        self.modal = None;
        self.profile = None;
        self.profile_denied = false;
        self.setup = Some(SetupState::new(&self.config, error));
        self.bookmarks.clear();
        self.selected_ids.clear();
        self.labels.clear();
        self.collections.clear();
        self.sidebar_items.clear();
        self.total = 0;
        self.loading_list = false;
        self.loading_more = false;
        self.list_error = None;
        self.list_state = ListState::default();
        self.sidebar_state = ListState::default();
        self.sidebar_search = TextInput::new("");
        self.sidebar_search_active = false;
    }

    // ------------------------------------------------------------------
    // Event handling
    // ------------------------------------------------------------------

    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Tick => {
                self.tick = self.tick.wrapping_add(1);
                if self
                    .toast
                    .as_ref()
                    .is_some_and(|toast| self.tick >= toast.expires)
                {
                    self.toast = None;
                }
            }
            Event::Resize => {}
            Event::Paste(text) => self.handle_paste(&text),
            Event::Key(key) => self.handle_key(key),
            Event::Api(api) => self.handle_api(*api),
        }
    }

    fn handle_paste(&mut self, text: &str) {
        if let Some(Modal::Input(input)) = &mut self.modal {
            input.input.insert_str(text);
            return;
        }
        if self.screen == Screen::Setup {
            if let Some(setup) = &mut self.setup {
                if !setup.busy {
                    setup.active_input_mut().insert_str(text);
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
        {
            self.quit = true;
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('t')
        {
            if self.modal.is_none() {
                self.open_theme_picker();
            }
            return;
        }

        if self.modal.is_some() {
            self.handle_modal_key(key);
            return;
        }

        if self.screen == Screen::Setup {
            self.handle_setup_key(key);
            return;
        }

        if key.code == KeyCode::Char('?') {
            self.modal = Some(Modal::Help);
            return;
        }

        if self.reader.is_some() {
            self.handle_reader_key(key);
            return;
        }

        match self.focus {
            Focus::Sidebar => self.handle_sidebar_key(key),
            Focus::List => self.handle_list_key(key),
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal.take() else {
            return;
        };

        match modal {
            Modal::Help => {
                if !matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
                ) {
                    self.modal = Some(Modal::Help);
                }
            }
            Modal::Confirm(confirm) => match key.code {
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {}
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.confirm(confirm.action);
                }
                _ => self.modal = Some(Modal::Confirm(confirm)),
            },
            Modal::Input(mut input) => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => self.submit_input(input),
                KeyCode::Tab
                    if matches!(input.kind, InputKind::EditLabels { .. }) =>
                {
                    self.complete_label(&mut input);
                    self.modal = Some(Modal::Input(input));
                }
                _ => {
                    let label_input =
                        matches!(input.kind, InputKind::EditLabels { .. });
                    input.input.handle_edit_key(&key);
                    if label_input {
                        self.refresh_label_suggestions(&mut input);
                    }
                    self.modal = Some(Modal::Input(input));
                }
            },
            Modal::ThemePicker(mut picker) => {
                let themes = crate::ui::theme::all();
                let len = themes.len().max(1);
                let mut close = false;
                let mut preview = false;
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        let _ = crate::ui::theme::set_name(&picker.original);
                        close = true;
                    }
                    KeyCode::Enter => {
                        if let Some((id, _)) = themes.get(picker.selected) {
                            let _ = crate::ui::theme::set_name(id);
                            self.config.theme = (*id).to_string();
                            let name = themes[picker.selected].1;
                            self.toast_info(format!("Theme: {name}"));
                        }
                        close = true;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        picker.selected = (picker.selected + 1) % len;
                        preview = true;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        picker.selected = (picker.selected + len - 1) % len;
                        preview = true;
                    }
                    KeyCode::PageDown => {
                        picker.selected = (picker.selected + 10).min(len - 1);
                        preview = true;
                    }
                    KeyCode::PageUp => {
                        picker.selected = picker.selected.saturating_sub(10);
                        preview = true;
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        picker.selected = 0;
                        preview = true;
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        picker.selected = len - 1;
                        preview = true;
                    }
                    _ => {}
                }

                if preview {
                    if let Some((id, _)) = themes.get(picker.selected) {
                        let _ = crate::ui::theme::set_name(id);
                    }
                }
                if !close {
                    self.modal = Some(Modal::ThemePicker(picker));
                }
            }
        }
    }

    fn handle_setup_key(&mut self, key: KeyEvent) {
        if self.oauth.is_some() {
            if key.code == KeyCode::Esc {
                self.cancel_oauth();
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('o')
        {
            self.start_oauth_login();
            return;
        }

        let mut submit = false;
        let mut quit = false;
        if let Some(setup) = &mut self.setup {
            if setup.busy {
                return;
            }
            match key.code {
                KeyCode::Esc => quit = true,
                KeyCode::Tab | KeyCode::Down => setup.next_field(),
                KeyCode::BackTab | KeyCode::Up => setup.previous_field(),
                KeyCode::Enter => submit = true,
                _ => {
                    setup.active_input_mut().handle_edit_key(&key);
                }
            }
        }
        if quit {
            self.quit = true;
        }
        if submit {
            self.submit_setup();
        }
    }

    fn submit_setup(&mut self) {
        let Some(setup) = &self.setup else {
            return;
        };
        if setup.busy {
            return;
        }
        let server = setup.server.value.trim().to_string();
        let token = setup.token.value.trim().to_string();

        if server.is_empty() {
            self.set_setup_error("Enter your Readeck server URL.");
            if let Some(setup) = &mut self.setup {
                setup.focus = SetupField::Server;
            }
            return;
        }
        if token.is_empty() {
            self.set_setup_error("Enter an API token.");
            if let Some(setup) = &mut self.setup {
                setup.focus = SetupField::Token;
            }
            return;
        }

        match ReadeckClient::new(&server, &token) {
            Ok(client) => {
                self.client = Some(client.clone());
                if let Some(setup) = &mut self.setup {
                    setup.busy = true;
                    setup.error = None;
                }
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(Event::api(ApiEvent::Setup(
                        client.profile().await,
                    )));
                });
            }
            Err(err) => self.set_setup_error(err.to_string()),
        }
    }

    fn start_oauth_login(&mut self) {
        if self.oauth.is_some() {
            return;
        }
        let Some(setup) = &self.setup else {
            return;
        };
        if setup.busy {
            return;
        }
        let server_input = setup.server.value.trim().to_string();
        if server_input.is_empty() {
            self.set_setup_error("Enter your Readeck server URL first.");
            if let Some(setup) = &mut self.setup {
                setup.focus = SetupField::Server;
            }
            return;
        }
        let server = match normalize_server(&server_input) {
            Ok(server) => server,
            Err(err) => {
                self.set_setup_error(err.to_string());
                return;
            }
        };
        let client = match ReadeckClient::new_oauth(&server) {
            Ok(client) => client,
            Err(err) => {
                self.set_setup_error(err.to_string());
                return;
            }
        };

        self.oauth_seq = self.oauth_seq.wrapping_add(1);
        let seq = self.oauth_seq;
        let cancel = Arc::new(AtomicBool::new(false));
        self.oauth = Some(OAuthState {
            server,
            device: None,
            error: None,
            cancel: cancel.clone(),
        });
        if let Some(setup) = &mut self.setup {
            setup.busy = true;
            setup.error = None;
        }

        let tx = self.tx.clone();
        tokio::spawn(async move {
            run_oauth_login(client, seq, cancel, tx).await;
        });
    }

    fn cancel_oauth(&mut self) {
        if let Some(oauth) = self.oauth.take() {
            oauth.cancel.store(true, Ordering::Relaxed);
        }
        self.oauth_seq = self.oauth_seq.wrapping_add(1);
        if let Some(setup) = &mut self.setup {
            setup.busy = false;
        }
    }

    fn set_setup_error(&mut self, message: impl Into<String>) {
        if let Some(setup) = &mut self.setup {
            setup.error = Some(message.into());
        }
    }

    fn complete_setup(
        &mut self,
        profile: Option<Profile>,
        profile_denied: bool,
    ) {
        let (server, token) = self
            .setup
            .as_ref()
            .map(|setup| {
                (
                    setup.server.value.trim().to_string(),
                    setup.token.value.trim().to_string(),
                )
            })
            .unwrap_or_default();
        self.finish_login(server, token, profile, profile_denied);
    }

    fn finish_login(
        &mut self,
        server: String,
        token: String,
        profile: Option<Profile>,
        profile_denied: bool,
    ) {
        let server = normalize_server(&server).unwrap_or(server);
        self.config.server = server;
        self.config.token = token;
        match ReadeckClient::new(&self.config.server, &self.config.token) {
            Ok(client) => self.client = Some(client),
            Err(err) => {
                self.show_setup(Some(err.to_string()));
                return;
            }
        }
        self.profile = profile;
        self.profile_denied = profile_denied;
        let save_result = self.config.save(&self.config_path);
        self.setup = None;
        self.oauth = None;
        self.enter_main();
        match save_result {
            Ok(()) if !self.profile_denied => {
                self.toast_success("Connected to Readeck")
            }
            Ok(()) => {
                self.toast_info("Connected (token has no profile access)")
            }
            Err(err) => self.toast_error(format!(
                "Connected, but could not save config: {err:#}"
            )),
        }
    }

    fn handle_sidebar_key(&mut self, key: KeyEvent) {
        if self.sidebar_search_active {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let half = (self.sidebar_viewport / 2).max(1);
                match key.code {
                    KeyCode::Char('d') => {
                        self.sidebar_move(half as isize);
                        return;
                    }
                    KeyCode::Char('u') => {
                        self.sidebar_move(-(half as isize));
                        return;
                    }
                    _ => {}
                }
            }

            match key.code {
                KeyCode::Esc => {
                    self.clear_sidebar_search();
                    return;
                }
                KeyCode::Enter => {
                    self.sidebar_search_active = false;
                    self.apply_sidebar_selection();
                    return;
                }
                KeyCode::Down => {
                    self.sidebar_move(1);
                    return;
                }
                KeyCode::Up => {
                    self.sidebar_move(-1);
                    return;
                }
                KeyCode::PageDown => {
                    let page = self.sidebar_viewport.max(1);
                    self.sidebar_move(page as isize);
                    return;
                }
                KeyCode::PageUp => {
                    let page = self.sidebar_viewport.max(1);
                    self.sidebar_move(-(page as isize));
                    return;
                }
                _ => {}
            }

            if self.sidebar_search.handle_edit_key(&key) {
                self.rebuild_sidebar();
            }
            return;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let half = (self.sidebar_viewport / 2).max(1);
            match key.code {
                KeyCode::Char('d') => {
                    self.sidebar_move(half as isize);
                    return;
                }
                KeyCode::Char('u') => {
                    self.sidebar_move(-(half as isize));
                    return;
                }
                _ => return,
            }
        }

        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc
            | KeyCode::Tab
            | KeyCode::Char('h')
            | KeyCode::Left => {
                if !self.sidebar_search.value.is_empty() {
                    self.clear_sidebar_search();
                } else {
                    self.focus = Focus::List;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.sidebar_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.sidebar_move(-1),
            KeyCode::PageDown => {
                let page = self.sidebar_viewport.max(1);
                self.sidebar_move(page as isize);
            }
            KeyCode::PageUp => {
                let page = self.sidebar_viewport.max(1);
                self.sidebar_move(-(page as isize));
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.sidebar_select_edge(true)
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.sidebar_select_edge(false)
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                self.apply_sidebar_selection()
            }
            KeyCode::Char('r') => self.refresh_all(),
            KeyCode::Char('/') => {
                self.sidebar_search_active = true;
                self.sidebar_search.cursor = self.sidebar_search.char_len();
            }
            _ => {}
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let half = (self.list_viewport / 2).max(1);
            match key.code {
                KeyCode::Char('d') => {
                    self.list_move(half as isize);
                    return;
                }
                KeyCode::Char('u') => {
                    self.list_move(-(half as isize));
                    return;
                }
                KeyCode::Char('a') => {
                    self.select_all_bookmarks();
                    return;
                }
                _ => return,
            }
        }

        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if !self.search.is_empty() {
                    self.search.clear();
                    self.selected_ids.clear();
                    self.request_bookmarks(false, false);
                    self.toast_info("Search cleared");
                } else if !self.selected_ids.is_empty() {
                    self.selected_ids.clear();
                    self.toast_info("Selection cleared");
                } else {
                    self.focus = Focus::Sidebar;
                    if !self.sidebar_items.is_empty()
                        && self.sidebar_state.selected().is_none()
                    {
                        self.sidebar_select_edge(true);
                    }
                }
            }
            KeyCode::Tab | KeyCode::Char('h') | KeyCode::Left => {
                self.focus = Focus::Sidebar;
                if !self.sidebar_items.is_empty()
                    && self.sidebar_state.selected().is_none()
                {
                    self.sidebar_select_edge(true);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.list_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.list_move(-1),
            KeyCode::Char(' ') => self.toggle_selection(),
            KeyCode::PageDown => {
                let page = self.list_viewport.max(1);
                self.list_move(page as isize);
            }
            KeyCode::PageUp => {
                let page = self.list_viewport.max(1);
                self.list_move(-(page as isize));
            }
            KeyCode::Char('g') | KeyCode::Home => self.list_select_edge(true),
            KeyCode::Char('G') | KeyCode::End => self.list_select_edge(false),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                self.open_reader()
            }
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Char('s') => self.cycle_sort(true),
            KeyCode::Char('S') => self.cycle_sort(false),
            KeyCode::Char('r') => self.refresh_all(),
            KeyCode::Char('n') => self.open_new_bookmark(),
            KeyCode::Char('o') => self.open_in_browser(),
            KeyCode::Char('m') => self.toggle_read(),
            KeyCode::Char('f') => self.toggle_favorite(),
            KeyCode::Char('a') => self.toggle_archive(),
            KeyCode::Char('D') => self.confirm_delete(),
            KeyCode::Char('t') => self.edit_labels(),
            KeyCode::Char('T') => self.edit_title(),
            KeyCode::Char('E') => self.edit_note(),
            _ => {}
        }
    }

    fn handle_reader_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let half = self
                .reader
                .as_ref()
                .map(Reader::half_page_amount)
                .unwrap_or(10);
            match key.code {
                KeyCode::Char('d') => {
                    self.reader_scroll(half as isize);
                    return;
                }
                KeyCode::Char('u') => {
                    self.reader_scroll(-(half as isize));
                    return;
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc => {
                let has_results = self.reader.as_ref().is_some_and(|reader| {
                    reader.search_query.is_some()
                        && !reader.search_match_lines().is_empty()
                });
                if has_results {
                    if let Some(reader) = &mut self.reader {
                        reader.search_query = None;
                    }
                    self.toast_info("Article search cleared");
                } else {
                    self.close_reader();
                }
            }
            KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Left => {
                self.close_reader();
            }
            KeyCode::Down | KeyCode::Char('j') => self.reader_scroll(1),
            KeyCode::Up | KeyCode::Char('k') => self.reader_scroll(-1),
            KeyCode::Char(' ') => {
                if let Some(reader) = &mut self.reader {
                    reader.scroll_to_next_paragraph();
                }
            }
            KeyCode::PageDown => {
                let amount =
                    self.reader.as_ref().map(Reader::page_amount).unwrap_or(20);
                self.reader_scroll(amount as isize);
            }
            KeyCode::PageUp | KeyCode::Char('b') => {
                let amount =
                    self.reader.as_ref().map(Reader::page_amount).unwrap_or(20);
                self.reader_scroll(-(amount as isize));
            }
            KeyCode::Char('g') | KeyCode::Home => {
                if let Some(reader) = &mut self.reader {
                    reader.jump_to(0);
                }
            }
            KeyCode::Char('G') | KeyCode::End => {
                if let Some(reader) = &mut self.reader {
                    let max = reader.max_scroll();
                    reader.jump_to(max);
                }
            }
            KeyCode::Char('m') => self.toggle_read(),
            KeyCode::Char('f') => self.toggle_favorite(),
            KeyCode::Char('a') => self.toggle_archive(),
            KeyCode::Char('o') => self.open_in_browser(),
            KeyCode::Char('t') => self.edit_labels(),
            KeyCode::Char('T') => self.edit_title(),
            KeyCode::Char('E') => self.edit_note(),
            KeyCode::Char('i') => self.toggle_reader_images(),
            KeyCode::Char('A') => self.toggle_reader_alignment(),
            KeyCode::Char('D') => self.confirm_delete(),
            KeyCode::Char('/') => self.open_reader_search(),
            KeyCode::Char('n') => {
                if let Some(reader) = &mut self.reader {
                    reader.next_search_match(true);
                }
            }
            KeyCode::Char('N') => {
                if let Some(reader) = &mut self.reader {
                    reader.next_search_match(false);
                }
            }
            KeyCode::Char('r') => self.retry_article(),
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // Bookmarks and reader
    // ------------------------------------------------------------------

    pub fn selected_bookmark(&self) -> Option<Bookmark> {
        let index = self.list_state.selected()?;
        self.bookmarks.get(index).cloned()
    }

    fn current_bookmark(&self) -> Option<Bookmark> {
        self.reader
            .as_ref()
            .map(|reader| reader.bookmark.clone())
            .or_else(|| self.selected_bookmark())
    }

    /// Bookmarks targeted by an action: the reader's bookmark, the current
    /// bulk selection, or the highlighted bookmark as a fallback.
    fn action_targets(&self) -> Vec<Bookmark> {
        if let Some(reader) = &self.reader {
            return vec![reader.bookmark.clone()];
        }
        if !self.selected_ids.is_empty() {
            let selected: Vec<Bookmark> = self
                .bookmarks
                .iter()
                .filter(|bookmark| self.selected_ids.contains(&bookmark.id))
                .cloned()
                .collect();
            if !selected.is_empty() {
                return selected;
            }
        }
        self.selected_bookmark().into_iter().collect()
    }

    fn toggle_selection(&mut self) {
        let Some(bookmark) = self.selected_bookmark() else {
            return;
        };
        if !self.selected_ids.remove(&bookmark.id) {
            self.selected_ids.insert(bookmark.id);
        }
    }

    fn select_all_bookmarks(&mut self) {
        if self.bookmarks.is_empty() {
            return;
        }
        self.selected_ids = self
            .bookmarks
            .iter()
            .map(|bookmark| bookmark.id.clone())
            .collect();
        self.toast_info(format!(
            "Selected {} bookmarks",
            self.selected_ids.len()
        ));
    }

    fn list_move(&mut self, delta: isize) {
        if self.bookmarks.is_empty() {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, self.bookmarks.len() as isize - 1)
            as usize;
        self.list_state.select(Some(next));
        self.maybe_load_more();
    }

    fn list_select_edge(&mut self, start: bool) {
        if self.bookmarks.is_empty() {
            return;
        }
        if start {
            self.list_state.select(Some(0));
            return;
        }
        self.list_state.select(Some(self.bookmarks.len() - 1));
        if self.bookmarks.len() < self.total {
            self.request_bookmarks(true, false);
        }
    }

    fn maybe_load_more(&mut self) {
        let Some(index) = self.list_state.selected() else {
            return;
        };
        if self.bookmarks.len() < self.total
            && index + 3 >= self.bookmarks.len()
            && !self.loading_more
            && !self.loading_list
        {
            self.request_bookmarks(true, false);
        }
    }

    fn request_bookmarks(&mut self, append: bool, preserve_selection: bool) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if append {
            if self.loading_more || self.loading_list {
                return;
            }
            if self.total == 0 || self.bookmarks.len() >= self.total {
                return;
            }
        } else {
            self.list_seq += 1;
            self.pending_select = if preserve_selection {
                self.selected_bookmark().map(|bookmark| bookmark.id)
            } else {
                None
            };
            if !preserve_selection {
                self.selected_ids.clear();
            }
            self.bookmarks.clear();
            self.total = 0;
            self.list_state = ListState::default();
            self.loading_list = true;
            self.loading_more = false;
            self.list_error = None;
        }

        let offset = if append { self.bookmarks.len() } else { 0 };
        let seq = self.list_seq;
        let query = BookmarkQuery::new(
            &self.filter,
            &self.search,
            self.sort,
            PAGE_SIZE,
            offset,
        );
        if append {
            self.loading_more = true;
        }

        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client.list_bookmarks(&query).await;
            let _ = tx.send(Event::api(ApiEvent::Bookmarks {
                seq,
                append,
                result,
            }));
        });
    }

    fn request_labels(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ =
                tx.send(Event::api(ApiEvent::Labels(client.labels().await)));
        });
    }

    fn request_collections(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Event::api(ApiEvent::Collections(
                client.collections().await,
            )));
        });
    }

    fn request_profile(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ =
                tx.send(Event::api(ApiEvent::Profile(client.profile().await)));
        });
    }

    fn open_reader(&mut self) {
        let Some(bookmark) = self.selected_bookmark() else {
            return;
        };
        let mut reader = Reader::new(
            bookmark.clone(),
            self.images_enabled,
            self.config.text_width,
            !self.config.text_align.trim().eq_ignore_ascii_case("left"),
        );
        if bookmark.has_article {
            self.reader = Some(reader);
            self.request_article(&bookmark.id);
        } else {
            reader.set_article(fallback_article(&bookmark));
            self.reader = Some(reader);
        }
    }

    fn request_article(&mut self, id: &str) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if let Some(reader) = &mut self.reader {
            reader.loading = true;
            reader.error = None;
        }
        let id = id.to_string();
        let tx = self.tx.clone();
        let requested = id.clone();
        tokio::spawn(async move {
            let result = match client.article(&requested).await {
                Ok(html) => Ok(parse_article(&html)),
                Err(err) => Err(err),
            };
            let _ = tx.send(Event::api(ApiEvent::Article { id, result }));
        });
    }

    fn request_images_for_reader(
        &mut self,
        bookmark_id: String,
        urls: Vec<String>,
    ) {
        let missing = {
            let Some(reader) = &mut self.reader else {
                return;
            };
            let missing: Vec<String> = urls
                .into_iter()
                .filter(|url| !reader.requested_images.contains(url))
                .collect();
            for url in &missing {
                reader.requested_images.insert(url.clone());
            }
            missing
        };
        if !missing.is_empty() {
            self.spawn_image_requests(bookmark_id, missing);
        }
    }

    fn spawn_image_requests(&mut self, bookmark_id: String, urls: Vec<String>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        for url in urls {
            let client = client.clone();
            let tx = self.tx.clone();
            let bookmark_id = bookmark_id.clone();
            let requested_url = url.clone();
            tokio::spawn(async move {
                let result = match client.fetch_media(&requested_url).await {
                    Ok(bytes) => image::load_from_memory(&bytes)
                        .map_err(|err| ApiError::Image(err.to_string())),
                    Err(err) => Err(err),
                };
                let _ = tx.send(Event::api(ApiEvent::Image {
                    bookmark_id,
                    url,
                    result,
                }));
            });
        }
    }

    fn toggle_reader_alignment(&mut self) {
        let Some(reader) = &mut self.reader else {
            return;
        };
        reader.center_text = !reader.center_text;
        let center = reader.center_text;
        self.config.text_align = if center {
            "center".to_string()
        } else {
            "left".to_string()
        };
        self.toast_info(format!(
            "Text alignment: {}",
            if center { "center" } else { "left" }
        ));
    }

    fn toggle_reader_images(&mut self) {
        self.images_enabled = !self.images_enabled;
        let enabled = self.images_enabled;

        let (bookmark_id, urls) = {
            let Some(reader) = &mut self.reader else {
                return;
            };
            reader.show_images = enabled;
            reader.invalidate_layout();
            let bookmark_id = reader.bookmark.id.clone();
            let urls = if enabled {
                reader
                    .article
                    .as_ref()
                    .map(|article| {
                        let mut urls = article
                            .images
                            .values()
                            .cloned()
                            .collect::<HashSet<_>>()
                            .into_iter()
                            .collect::<Vec<_>>();
                        urls.sort();
                        urls.truncate(12);
                        urls
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            (bookmark_id, urls)
        };

        if enabled {
            self.request_images_for_reader(bookmark_id, urls);
        }
        self.toast_info(if enabled {
            "Images enabled"
        } else {
            "Images disabled"
        });
    }

    fn retry_article(&mut self) {
        let Some(reader) = &self.reader else {
            return;
        };
        if reader.loading {
            return;
        }
        let id = reader.bookmark.id.clone();
        self.request_article(&id);
    }

    fn close_reader(&mut self) {
        let Some(reader) = self.reader.take() else {
            return;
        };
        let progress = reader.progress();
        if reader.scrolled
            && !reader.manual_read_state
            && !reader.loading
            && reader.error.is_none()
            && progress != reader.initial_progress
        {
            let patch = BookmarkPatch {
                read_progress: Some(progress),
                ..Default::default()
            };
            self.update_bookmark(reader.bookmark.id, patch, "", true);
        }
    }

    fn reader_scroll(&mut self, delta: isize) {
        if let Some(reader) = &mut self.reader {
            reader.scroll_by(delta);
        }
    }

    // ------------------------------------------------------------------
    // Bookmark actions
    // ------------------------------------------------------------------

    fn toggle_read(&mut self) {
        let targets = self.action_targets();
        if targets.is_empty() {
            return;
        }
        let mark_unread = targets.iter().all(Bookmark::is_read);
        let count = targets.len();
        let patch = BookmarkPatch {
            read_progress: Some(if mark_unread { 0 } else { 100 }),
            ..Default::default()
        };
        let message = match (mark_unread, count) {
            (true, 1) => "Marked as unread".to_string(),
            (true, n) => format!("Marked {n} bookmarks as unread"),
            (false, 1) => "Marked as read".to_string(),
            (false, n) => format!("Marked {n} bookmarks as read"),
        };
        let ids = targets.into_iter().map(|bookmark| bookmark.id).collect();
        self.update_ids(ids, patch, message, false);
    }

    fn toggle_favorite(&mut self) {
        let targets = self.action_targets();
        if targets.is_empty() {
            return;
        }
        let remove = targets.iter().all(|bookmark| bookmark.is_marked);
        let count = targets.len();
        let patch = BookmarkPatch {
            is_marked: Some(!remove),
            ..Default::default()
        };
        let message = match (remove, count) {
            (true, 1) => "Removed from favorites".to_string(),
            (true, n) => format!("Removed {n} bookmarks from favorites"),
            (false, 1) => "Added to favorites".to_string(),
            (false, n) => format!("Added {n} bookmarks to favorites"),
        };
        let ids = targets.into_iter().map(|bookmark| bookmark.id).collect();
        self.update_ids(ids, patch, message, false);
    }

    fn toggle_archive(&mut self) {
        let targets = self.action_targets();
        if targets.is_empty() {
            return;
        }
        let unarchive = targets.iter().all(|bookmark| bookmark.is_archived);
        let count = targets.len();
        let patch = BookmarkPatch {
            is_archived: Some(!unarchive),
            ..Default::default()
        };
        let message = match (unarchive, count) {
            (true, 1) => "Bookmark restored from archive".to_string(),
            (true, n) => format!("Restored {n} bookmarks from archive"),
            (false, 1) => "Bookmark archived".to_string(),
            (false, n) => format!("Archived {n} bookmarks"),
        };
        let ids = targets.into_iter().map(|bookmark| bookmark.id).collect();
        self.update_ids(ids, patch, message, false);
    }

    fn update_ids(
        &mut self,
        ids: Vec<String>,
        patch: BookmarkPatch,
        message: impl Into<String>,
        quiet: bool,
    ) {
        let message = message.into();
        if ids.is_empty() || patch.is_empty() {
            return;
        }
        if ids.len() == 1 && self.selected_ids.is_empty() {
            self.update_bookmark(
                ids.into_iter().next().unwrap(),
                patch,
                message,
                quiet,
            );
        } else {
            self.update_bookmarks_bulk(ids, patch, message, quiet);
        }
    }

    fn update_bookmarks_bulk(
        &mut self,
        ids: Vec<String>,
        patch: BookmarkPatch,
        message: String,
        quiet: bool,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if !quiet {
            self.toast_info(format!("Updating {} bookmarks…", ids.len()));
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client.update_bookmarks(&ids, &patch).await;
            let _ = tx.send(Event::api(ApiEvent::BulkUpdated {
                ids,
                patch,
                message,
                quiet,
                result,
            }));
        });
    }

    fn update_bookmark(
        &mut self,
        id: String,
        patch: BookmarkPatch,
        message: impl Into<String>,
        quiet: bool,
    ) {
        if patch.is_empty() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let message = message.into();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client.update_bookmark(&id, &patch).await;
            let _ = tx.send(Event::api(ApiEvent::Updated {
                id,
                patch,
                message,
                quiet,
                result,
            }));
        });
    }

    fn confirm_delete(&mut self) {
        let targets = self.action_targets();
        if targets.is_empty() {
            return;
        }
        let count = targets.len();
        let ids = targets
            .iter()
            .map(|bookmark| bookmark.id.clone())
            .collect::<Vec<_>>();
        let (title, message) = if count == 1 {
            let title = targets[0].display_title();
            (
                "Delete bookmark".to_string(),
                format!(
                    "Delete “{title}”?\nThis removes the bookmark and its article from Readeck."
                ),
            )
        } else {
            (
                format!("Delete {count} bookmarks"),
                format!(
                    "Delete {count} selected bookmarks?\nThis removes them and their articles from Readeck."
                ),
            )
        };
        self.modal = Some(Modal::Confirm(ConfirmModal {
            title,
            message,
            action: ConfirmAction::DeleteBookmarks { ids },
        }));
    }

    fn confirm(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::DeleteBookmarks { ids } => self.start_delete(ids),
        }
    }

    fn start_delete(&mut self, ids: Vec<String>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if ids.len() == 1 && self.selected_ids.is_empty() {
            let id = ids.into_iter().next().unwrap();
            let title = self
                .bookmarks
                .iter()
                .find(|bookmark| bookmark.id == id)
                .map(Bookmark::display_title)
                .unwrap_or_else(|| "bookmark".to_string());
            let tx = self.tx.clone();
            let deleted_id = id.clone();
            tokio::spawn(async move {
                let result = client.delete_bookmark(&deleted_id).await;
                let _ = tx.send(Event::api(ApiEvent::Deleted { id, result }));
            });
            self.toast_info(format!("Deleting “{title}”…"));
        } else {
            let count = ids.len();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let result = client.delete_bookmarks(&ids).await;
                let _ =
                    tx.send(Event::api(ApiEvent::BulkDeleted { ids, result }));
            });
            if count == 1 {
                self.toast_info("Deleting bookmark…");
            } else {
                self.toast_info(format!("Deleting {count} bookmarks…"));
            }
        }
    }

    fn open_in_browser(&mut self) {
        let Some(bookmark) = self.current_bookmark() else {
            return;
        };
        if bookmark.url.trim().is_empty() {
            self.toast_error("This bookmark has no URL.");
            return;
        }
        if let Err(err) = open::that(&bookmark.url) {
            self.toast_error(format!("Could not open browser: {err}"));
        }
    }

    fn open_new_bookmark(&mut self) {
        self.modal = Some(Modal::Input(InputModal::new(
            "Add bookmark",
            "URL to save in Readeck",
            TextInput::new(""),
            InputKind::NewBookmark,
        )));
    }

    fn open_search(&mut self) {
        self.modal = Some(Modal::Input(InputModal::new(
            "Search",
            "Full text search (empty clears)",
            TextInput::new(&self.search),
            InputKind::Search,
        )));
    }

    fn open_reader_search(&mut self) {
        let Some(reader) = &self.reader else {
            return;
        };
        let prefill = reader.search_query.clone().unwrap_or_default();
        self.modal = Some(Modal::Input(InputModal::new(
            "Search article",
            "Search this article (empty clears)",
            TextInput::new(prefill),
            InputKind::ReaderSearch,
        )));
    }

    fn edit_labels(&mut self) {
        let targets = self.action_targets();
        if targets.is_empty() {
            return;
        }
        let count = targets.len();
        let ids = targets
            .iter()
            .map(|bookmark| bookmark.id.clone())
            .collect::<Vec<_>>();

        let sorted_labels = targets
            .iter()
            .map(|bookmark| {
                let mut labels = bookmark.labels.clone();
                labels.sort();
                labels
            })
            .collect::<Vec<_>>();
        let labels_match =
            sorted_labels.windows(2).all(|pair| pair[0] == pair[1]);
        let prefill = if labels_match {
            targets[0].labels.join(", ")
        } else {
            String::new()
        };
        let prompt = if count > 1 && !labels_match {
            format!(
                "Replace labels on {count} bookmarks (current labels differ)"
            )
        } else {
            "Comma separated labels".to_string()
        };
        let title = if count > 1 {
            format!("Edit labels for {count} bookmarks")
        } else {
            "Edit labels".to_string()
        };

        let mut modal = InputModal::new(
            title,
            prompt,
            TextInput::new(prefill),
            InputKind::EditLabels { ids },
        );
        self.refresh_label_suggestions(&mut modal);
        self.modal = Some(Modal::Input(modal));
    }

    fn refresh_label_suggestions(&self, modal: &mut InputModal) {
        if !matches!(modal.kind, InputKind::EditLabels { .. }) {
            modal.suggestions.clear();
            modal.completion = None;
            return;
        }
        modal.suggestions = label_suggestions(
            &self.labels,
            &modal.input.value,
            modal.input.cursor,
        );
        modal.completion = None;
    }

    fn complete_label(&self, modal: &mut InputModal) {
        if let Some(completion) = modal.completion.clone() {
            if completion.candidates.is_empty() {
                return;
            }
            let next = (completion.index + 1) % completion.candidates.len();
            let label = completion.candidates[next].clone();
            modal
                .input
                .replace_range(completion.start, completion.end, &label);
            let end = completion.start + label.chars().count();
            modal.completion = Some(LabelCompletion {
                candidates: completion.candidates,
                index: next,
                start: completion.start,
                end,
            });
            return;
        }

        let candidates = label_suggestions(
            &self.labels,
            &modal.input.value,
            modal.input.cursor,
        );
        if candidates.is_empty() {
            return;
        }
        let (start, end, _) =
            label_token(&modal.input.value, modal.input.cursor);
        let label = candidates[0].clone();
        modal.input.replace_range(start, end, &label);
        let end = start + label.chars().count();
        modal.suggestions = candidates.clone();
        modal.completion = Some(LabelCompletion {
            candidates,
            index: 0,
            start,
            end,
        });
    }

    fn edit_note(&mut self) {
        let Some(bookmark) = self.current_bookmark() else {
            return;
        };
        let note = bookmark.note.clone().unwrap_or_default();
        if note.contains('\n') {
            self.toast_info(
                "Multi-line notes can be edited in the Readeck web UI.",
            );
            return;
        }
        self.modal = Some(Modal::Input(InputModal::new(
            "Edit note",
            "Note (empty clears it)",
            TextInput::new(note),
            InputKind::EditNote { id: bookmark.id },
        )));
    }

    fn edit_title(&mut self) {
        let Some(bookmark) = self.current_bookmark() else {
            return;
        };
        self.modal = Some(Modal::Input(InputModal::new(
            "Edit title",
            "Bookmark title",
            TextInput::new(bookmark.display_title()),
            InputKind::EditTitle { id: bookmark.id },
        )));
    }

    fn submit_input(&mut self, modal: InputModal) {
        let value = modal.input.value.trim().to_string();
        match modal.kind {
            InputKind::Search => {
                self.search = value.clone();
                self.request_bookmarks(false, false);
                if self.search.is_empty() {
                    self.toast_info("Search cleared");
                } else {
                    self.toast_info(format!("Search: “{}”", self.search));
                }
            }
            InputKind::ReaderSearch => {
                let Some(reader) = &mut self.reader else {
                    return;
                };
                let count = reader.search(&value);
                if value.is_empty() {
                    self.toast_info("Article search cleared");
                } else if count == 0 {
                    self.toast_info(format!("No matches for “{value}”"));
                } else {
                    self.toast_info(format!("{count} matches for “{value}”"));
                }
            }
            InputKind::NewBookmark => {
                if value.is_empty() {
                    self.toast_error("A URL is required.");
                    return;
                }
                self.create_bookmark(&value);
            }
            InputKind::EditLabels { ids } => {
                let labels = parse_labels(&value);
                let count = ids.len();
                let patch = BookmarkPatch {
                    labels: Some(labels.clone()),
                    ..Default::default()
                };
                let message = if labels.is_empty() {
                    if count > 1 {
                        format!("Labels cleared for {count} bookmarks")
                    } else {
                        "Labels cleared".to_string()
                    }
                } else if count > 1 {
                    format!("Labels updated for {count} bookmarks")
                } else {
                    format!("Labels: {}", labels.join(", "))
                };
                self.update_ids(ids, patch, message, false);
            }
            InputKind::EditNote { id } => {
                let patch = BookmarkPatch {
                    note: Some(value),
                    ..Default::default()
                };
                self.update_bookmark(id, patch, "Note updated", false);
            }
            InputKind::EditTitle { id } => {
                if value.is_empty() {
                    self.toast_error("The title cannot be empty.");
                    return;
                }
                let patch = BookmarkPatch {
                    title: Some(value),
                    ..Default::default()
                };
                self.update_bookmark(id, patch, "Title updated", false);
            }
        }
    }

    fn create_bookmark(&mut self, input: &str) {
        let url = match normalize_url(input) {
            Ok(url) => url,
            Err(message) => {
                self.toast_error(message);
                return;
            }
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Event::api(ApiEvent::Created(
                client.create_bookmark(&url).await,
            )));
        });
        self.toast_info("Saving bookmark…");
    }

    fn cycle_sort(&mut self, forward: bool) {
        self.sort = if forward {
            self.sort.next()
        } else {
            self.sort.previous()
        };
        self.request_bookmarks(false, false);
        self.toast_info(format!("Sort: {}", self.sort.label()));
    }

    fn refresh_all(&mut self) {
        self.request_bookmarks(false, true);
        self.request_labels();
        self.request_collections();
        if !self.profile_denied {
            self.request_profile();
        }
        self.toast_info("Refreshing…");
    }

    // ------------------------------------------------------------------
    // Sidebar
    // ------------------------------------------------------------------

    fn rebuild_sidebar(&mut self) {
        let current = self
            .sidebar_items
            .get(self.sidebar_state.selected().unwrap_or(0))
            .and_then(|item| item.filter.clone());
        let query = self.sidebar_search.value.trim().to_lowercase();
        let matches = |text: &str| {
            query.is_empty() || text.to_lowercase().contains(&query)
        };

        let mut items = Vec::new();
        for (filter, text) in [
            (Filter::Unread, "●  Unread"),
            (Filter::Reading, "◐  Reading"),
            (Filter::Marked, "★  Favorites"),
            (Filter::Archived, "■  Archived"),
            (Filter::All, "≡  All Bookmarks"),
        ] {
            if matches(text) {
                items.push(SidebarItem {
                    filter: Some(filter),
                    text: text.into(),
                    count: None,
                    header: false,
                });
            }
        }

        let labels: Vec<&Label> = self
            .labels
            .iter()
            .filter(|label| {
                !label.name.trim().is_empty() && matches(&label.name)
            })
            .collect();
        if !labels.is_empty() {
            items.push(SidebarItem {
                filter: None,
                text: "LABELS".into(),
                count: None,
                header: true,
            });
            for label in labels {
                items.push(SidebarItem {
                    filter: Some(Filter::Label(label.name.clone())),
                    text: format!("# {}", label.name),
                    count: Some(label.count),
                    header: false,
                });
            }
        }

        let mut collections: Vec<&Collection> = self
            .collections
            .iter()
            .filter(|collection| {
                !collection.name.trim().is_empty()
                    && !collection.is_deleted
                    && matches(&collection.name)
            })
            .collect();
        collections.sort_by_key(|collection| {
            (!collection.is_pinned, collection.name.to_lowercase())
        });
        if !collections.is_empty() {
            items.push(SidebarItem {
                filter: None,
                text: "COLLECTIONS".into(),
                count: None,
                header: true,
            });
            for collection in collections {
                items.push(SidebarItem {
                    filter: Some(Filter::Collection {
                        id: collection.id.clone(),
                        name: collection.name.clone(),
                    }),
                    text: format!("▤  {}", collection.name),
                    count: None,
                    header: false,
                });
            }
        }

        let target = current
            .and_then(|filter| {
                items
                    .iter()
                    .position(|item| item.filter.as_ref() == Some(&filter))
            })
            .or_else(|| {
                items
                    .iter()
                    .position(|item| item.filter.as_ref() == Some(&self.filter))
            })
            .or_else(|| items.iter().position(|item| !item.header));

        self.sidebar_items = items;
        self.sidebar_state.select(target);
    }

    fn clear_sidebar_search(&mut self) {
        self.sidebar_search = TextInput::new("");
        self.sidebar_search_active = false;
        self.rebuild_sidebar();
    }

    fn sidebar_move(&mut self, delta: isize) {
        if self.sidebar_items.is_empty() {
            return;
        }
        let len = self.sidebar_items.len() as isize;
        let mut index = self.sidebar_state.selected().unwrap_or(0) as isize;
        for _ in 0..len {
            index = (index + delta).rem_euclid(len);
            if !self.sidebar_items[index as usize].header {
                self.sidebar_state.select(Some(index as usize));
                return;
            }
        }
    }

    fn sidebar_select_edge(&mut self, start: bool) {
        if start {
            if let Some(index) =
                self.sidebar_items.iter().position(|item| !item.header)
            {
                self.sidebar_state.select(Some(index));
            }
        } else if let Some(index) =
            self.sidebar_items.iter().rposition(|item| !item.header)
        {
            self.sidebar_state.select(Some(index));
        }
    }

    fn apply_sidebar_selection(&mut self) {
        let Some(index) = self.sidebar_state.selected() else {
            return;
        };
        let Some(filter) = self
            .sidebar_items
            .get(index)
            .and_then(|item| item.filter.clone())
        else {
            return;
        };
        let changed = filter != self.filter;
        self.filter = filter;
        self.focus = Focus::List;
        self.sidebar_search = TextInput::new("");
        self.sidebar_search_active = false;
        self.rebuild_sidebar();
        if changed {
            self.request_bookmarks(false, false);
        }
    }

    // ------------------------------------------------------------------
    // API results
    // ------------------------------------------------------------------

    fn handle_api(&mut self, event: ApiEvent) {
        if self.screen == Screen::Setup
            && !matches!(
                event,
                ApiEvent::Setup(_)
                    | ApiEvent::OAuthDevice { .. }
                    | ApiEvent::OAuthToken { .. }
            )
        {
            return;
        }

        match event {
            ApiEvent::Bookmarks {
                seq,
                append,
                result,
            } => self.handle_bookmarks(seq, append, result),
            ApiEvent::Labels(result) => match result {
                Ok(labels) => {
                    self.labels = labels
                        .into_iter()
                        .filter(|label| !label.name.trim().is_empty())
                        .collect();
                    self.rebuild_sidebar();
                }
                Err(err) => {
                    if !self.handle_unauthorized(&err) {
                        self.toast_error(err.to_string());
                    }
                }
            },
            ApiEvent::Collections(result) => match result {
                Ok(collections) => {
                    self.collections = collections;
                    self.rebuild_sidebar();
                }
                Err(err) => {
                    if !self.handle_unauthorized(&err) {
                        self.toast_error(err.to_string());
                    }
                }
            },
            ApiEvent::Profile(result) => {
                match result {
                    Ok(profile) => {
                        self.profile = Some(profile);
                        self.profile_denied = false;
                    }
                    Err(err) => {
                        if err.status() == Some(403) {
                            self.profile_denied = true;
                            self.toast_info("API token has no profile access; username hidden.");
                        } else if !self.handle_unauthorized(&err) {
                            self.toast_error(err.to_string());
                        }
                    }
                }
            }
            ApiEvent::Article { id, result } => {
                if !self
                    .reader
                    .as_ref()
                    .is_some_and(|reader| reader.bookmark.id == id)
                {
                    return;
                }
                match result {
                    Ok(article) => {
                        let urls = article
                            .images
                            .values()
                            .cloned()
                            .collect::<HashSet<_>>()
                            .into_iter()
                            .take(12)
                            .collect::<Vec<_>>();
                        if let Some(reader) = &mut self.reader {
                            reader.set_article(article);
                        }
                        if self.images_enabled {
                            self.request_images_for_reader(id, urls);
                        }
                    }
                    Err(err) => {
                        if !self.handle_unauthorized(&err) {
                            if let Some(reader) = &mut self.reader {
                                reader.loading = false;
                                reader.error = Some(article_error(&err));
                            }
                        }
                    }
                }
            }
            ApiEvent::Image {
                bookmark_id,
                url,
                result,
            } => {
                if !self
                    .reader
                    .as_ref()
                    .is_some_and(|reader| reader.bookmark.id == bookmark_id)
                {
                    return;
                }
                if let Some(reader) = &mut self.reader {
                    match result {
                        Ok(image) => reader.set_image(url, image),
                        Err(err) => {
                            reader.set_image_error(url, err.to_string())
                        }
                    }
                }
            }
            ApiEvent::Updated {
                id,
                patch,
                message,
                quiet,
                result,
            } => match result {
                Ok(()) => {
                    self.apply_patch(&id, &patch);
                    if !quiet {
                        self.toast_success(message);
                    }
                }
                Err(err) => {
                    if !self.handle_unauthorized(&err) {
                        self.toast_error(err.to_string());
                        self.request_bookmarks(false, true);
                    }
                }
            },
            ApiEvent::BulkUpdated {
                ids,
                patch,
                message,
                quiet,
                result,
            } => match result {
                Ok(_) => {
                    self.apply_patch_to_ids(&ids, &patch);
                    if !quiet {
                        self.toast_success(message);
                    }
                }
                Err(err) => {
                    if !self.handle_unauthorized(&err) {
                        self.toast_error(err.to_string());
                        self.request_bookmarks(false, true);
                    }
                }
            },
            ApiEvent::Created(result) => match result {
                Ok(_) => {
                    self.toast_success("Bookmark saved");
                    self.request_bookmarks(false, true);
                }
                Err(err) => {
                    if !self.handle_unauthorized(&err) {
                        self.toast_error(err.to_string());
                    }
                }
            },
            ApiEvent::Deleted { id, result } => match result {
                Ok(()) => {
                    self.remove_bookmark(&id);
                    self.toast_success("Bookmark deleted");
                }
                Err(err) => {
                    if !self.handle_unauthorized(&err) {
                        self.toast_error(err.to_string());
                    }
                }
            },
            ApiEvent::BulkDeleted { ids, result } => match result {
                Ok(_) => {
                    let count = ids.len();
                    self.remove_bookmarks(&ids);
                    if count == 1 {
                        self.toast_success("Bookmark deleted");
                    } else {
                        self.toast_success(format!(
                            "Deleted {count} bookmarks"
                        ));
                    }
                }
                Err(err) => {
                    if !self.handle_unauthorized(&err) {
                        self.toast_error(err.to_string());
                        self.request_bookmarks(false, true);
                    }
                }
            },
            ApiEvent::OAuthDevice { seq, result } => {
                if seq != self.oauth_seq {
                    return;
                }
                if let Some(oauth) = &mut self.oauth {
                    match result {
                        Ok(device) => oauth.device = Some(device),
                        Err(err) => {
                            oauth.error = Some(err.to_string());
                            if let Some(setup) = &mut self.setup {
                                setup.busy = false;
                            }
                        }
                    }
                }
            }
            ApiEvent::OAuthToken { seq, result } => {
                if seq != self.oauth_seq {
                    return;
                }
                match result {
                    Ok(token) => {
                        let server = self
                            .oauth
                            .as_ref()
                            .map(|oauth| oauth.server.clone())
                            .unwrap_or_default();
                        self.finish_login(server, token, None, false);
                    }
                    Err(err) => {
                        if let Some(oauth) = &mut self.oauth {
                            oauth.error = Some(err.to_string());
                        }
                        if let Some(setup) = &mut self.setup {
                            setup.busy = false;
                        }
                    }
                }
            }
            ApiEvent::Setup(result) => match result {
                Ok(profile) => self.complete_setup(Some(profile), false),
                Err(err) if err.status() == Some(403) => {
                    // A valid token may be restricted to bookmarks only. The
                    // profile is used solely for the username in the title bar.
                    self.complete_setup(None, true);
                }
                Err(err) => {
                    self.client = None;
                    if let Some(setup) = &mut self.setup {
                        setup.busy = false;
                        setup.error = Some(err.to_string());
                    }
                }
            },
        }
    }

    fn handle_bookmarks(
        &mut self,
        seq: u64,
        append: bool,
        result: Result<crate::models::Page<Bookmark>, ApiError>,
    ) {
        if seq != self.list_seq {
            return;
        }
        self.loading_list = false;
        self.loading_more = false;

        match result {
            Ok(page) => {
                if append {
                    for bookmark in page.items {
                        if !self
                            .bookmarks
                            .iter()
                            .any(|existing| existing.id == bookmark.id)
                        {
                            self.bookmarks.push(bookmark);
                        }
                    }
                    self.total =
                        self.total.max(page.total).max(self.bookmarks.len());
                } else {
                    self.bookmarks = page.items;
                    self.total = page.total.max(self.bookmarks.len());
                    let index = self
                        .pending_select
                        .take()
                        .and_then(|id| {
                            self.bookmarks.iter().position(|b| b.id == id)
                        })
                        .or_else(|| (!self.bookmarks.is_empty()).then_some(0));
                    self.list_state.select(index);
                }
                self.selected_ids.retain(|id| {
                    self.bookmarks.iter().any(|bookmark| &bookmark.id == id)
                });
            }
            Err(err) => {
                if self.handle_unauthorized(&err) {
                    return;
                }
                if append {
                    self.toast_error(err.to_string());
                } else {
                    let message = err.to_string();
                    self.list_error = Some(message.clone());
                    self.toast_error(message);
                }
            }
        }
    }

    fn apply_patch(&mut self, id: &str, patch: &BookmarkPatch) {
        if let Some(bookmark) = self.bookmarks.iter_mut().find(|b| b.id == id) {
            bookmark.apply_patch(patch);
        }
        if let Some(reader) = &mut self.reader {
            if reader.bookmark.id == id {
                reader.bookmark.apply_patch(patch);
                if let Some(progress) = patch.read_progress {
                    reader.initial_progress = progress;
                    reader.manual_read_state = true;
                }
            }
        }

        let filter = self.filter.clone();
        let before = self.bookmarks.len();
        self.bookmarks.retain(|bookmark| {
            bookmark.id != id || keeps_filter(&filter, bookmark)
        });
        if self.bookmarks.len() < before {
            self.total =
                self.total.saturating_sub(before - self.bookmarks.len());
            self.clamp_selection();
        }
        self.selected_ids.retain(|selected| {
            self.bookmarks
                .iter()
                .any(|bookmark| &bookmark.id == selected)
        });
    }

    fn apply_patch_to_ids(&mut self, ids: &[String], patch: &BookmarkPatch) {
        let id_set: HashSet<&str> = ids.iter().map(String::as_str).collect();
        for bookmark in &mut self.bookmarks {
            if id_set.contains(bookmark.id.as_str()) {
                bookmark.apply_patch(patch);
            }
        }
        if let Some(reader) = &mut self.reader {
            if id_set.contains(reader.bookmark.id.as_str()) {
                reader.bookmark.apply_patch(patch);
                if let Some(progress) = patch.read_progress {
                    reader.initial_progress = progress;
                    reader.manual_read_state = true;
                }
            }
        }

        let filter = self.filter.clone();
        let before = self.bookmarks.len();
        self.bookmarks.retain(|bookmark| {
            !id_set.contains(bookmark.id.as_str())
                || keeps_filter(&filter, bookmark)
        });
        if self.bookmarks.len() < before {
            self.total =
                self.total.saturating_sub(before - self.bookmarks.len());
            self.clamp_selection();
        }
        let visible: HashSet<&str> = self
            .bookmarks
            .iter()
            .map(|bookmark| bookmark.id.as_str())
            .collect();
        self.selected_ids
            .retain(|selected| visible.contains(selected.as_str()));
    }

    fn remove_bookmark(&mut self, id: &str) {
        self.remove_bookmarks(&[id.to_string()]);
    }

    fn remove_bookmarks(&mut self, ids: &[String]) {
        let id_set: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let before = self.bookmarks.len();
        self.bookmarks
            .retain(|bookmark| !id_set.contains(bookmark.id.as_str()));
        if self.bookmarks.len() < before {
            self.total =
                self.total.saturating_sub(before - self.bookmarks.len());
            self.clamp_selection();
        }
        self.selected_ids
            .retain(|selected| !id_set.contains(selected.as_str()));
        if self
            .reader
            .as_ref()
            .is_some_and(|reader| id_set.contains(reader.bookmark.id.as_str()))
        {
            self.reader = None;
        }
    }

    fn clamp_selection(&mut self) {
        let len = self.bookmarks.len();
        let index = match self.list_state.selected() {
            Some(index) if len > 0 => Some(index.min(len - 1)),
            Some(_) => None,
            None if len > 0 => Some(0),
            None => None,
        };
        self.list_state.select(index);
    }

    fn handle_unauthorized(&mut self, err: &ApiError) -> bool {
        if err.status() == Some(401) {
            self.show_setup(Some(err.to_string()));
            true
        } else {
            false
        }
    }

    fn open_theme_picker(&mut self) {
        let current = crate::ui::theme::current_info();
        let themes = crate::ui::theme::all();
        let selected = themes
            .iter()
            .position(|(id, _)| *id == current.id)
            .unwrap_or(0);
        self.modal = Some(Modal::ThemePicker(ThemePicker {
            selected,
            original: current.id,
        }));
    }

    // ------------------------------------------------------------------
    // Toasts
    // ------------------------------------------------------------------

    pub fn toast_info(&mut self, message: impl Into<String>) {
        self.set_toast(ToastKind::Info, message.into(), 50);
    }

    pub fn toast_success(&mut self, message: impl Into<String>) {
        self.set_toast(ToastKind::Success, message.into(), 40);
    }

    pub fn toast_error(&mut self, message: impl Into<String>) {
        self.set_toast(ToastKind::Error, message.into(), 120);
    }

    fn set_toast(&mut self, kind: ToastKind, text: String, ttl: u64) {
        self.toast = Some(Toast {
            kind,
            text,
            expires: self.tick.wrapping_add(ttl),
        });
    }
}

fn keeps_filter(filter: &Filter, bookmark: &Bookmark) -> bool {
    match filter {
        Filter::All => true,
        Filter::Unread => !bookmark.is_archived,
        Filter::Reading => {
            (1..100).contains(&bookmark.read_progress) && !bookmark.is_archived
        }
        Filter::Marked => bookmark.is_marked && !bookmark.is_archived,
        Filter::Archived => bookmark.is_archived,
        Filter::Label(name) => bookmark.labels.iter().any(|l| l == name),
        Filter::Collection { .. } => true,
    }
}

/// Locate the comma-separated label token around the cursor.
///
/// Returns character indices for the trimmed token and the text before the
/// cursor, which is used as the completion prefix.
fn label_token(value: &str, cursor: usize) -> (usize, usize, String) {
    let chars: Vec<char> = value.chars().collect();
    let cursor = cursor.min(chars.len());

    let mut start = 0;
    for index in (0..cursor).rev() {
        if chars[index] == ',' {
            start = index + 1;
            break;
        }
    }

    let mut end = cursor;
    while end < chars.len() && chars[end] != ',' {
        end += 1;
    }

    let mut content_start = start;
    while content_start < cursor && chars[content_start].is_whitespace() {
        content_start += 1;
    }
    let mut content_end = end;
    while content_end > content_start && chars[content_end - 1].is_whitespace()
    {
        content_end -= 1;
    }

    let prefix_end = cursor.min(content_end).max(content_start);
    let prefix = chars[content_start..prefix_end]
        .iter()
        .collect::<String>()
        .to_lowercase();
    (content_start, content_end, prefix)
}

fn label_suggestions(
    labels: &[Label],
    value: &str,
    cursor: usize,
) -> Vec<String> {
    let chars: Vec<char> = value.chars().collect();
    let cursor = cursor.min(chars.len());
    let (content_start, content_end, _) = label_token(value, cursor);
    let prefix_end = cursor.min(content_end).max(content_start);
    let prefix: String = chars[content_start..prefix_end]
        .iter()
        .collect::<String>()
        .to_lowercase();

    let mut used = HashSet::new();
    let mut token_start = 0;
    for index in 0..=chars.len() {
        if index == chars.len() || chars[index] == ',' {
            if !(token_start <= cursor && cursor <= index) {
                let token: String = chars[token_start..index].iter().collect();
                let token = token.trim();
                if !token.is_empty() {
                    used.insert(token.to_lowercase());
                }
            }
            token_start = index + 1;
        }
    }

    let mut suggestions: Vec<String> = labels
        .iter()
        .map(|label| label.name.trim())
        .filter(|name| !name.is_empty())
        .filter(|name| !used.contains(&name.to_lowercase()))
        .filter(|name| name.to_lowercase().starts_with(&prefix))
        .map(str::to_string)
        .collect();
    suggestions.sort_by_key(|name| name.to_lowercase());
    suggestions.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    suggestions
}

fn parse_labels(value: &str) -> Vec<String> {
    let mut labels = Vec::new();
    for part in value.split(',') {
        let label = part.trim();
        if !label.is_empty()
            && !labels.iter().any(|existing: &String| existing == label)
        {
            labels.push(label.to_string());
        }
    }
    labels
}

pub fn normalize_url(input: &str) -> Result<String, String> {
    let mut value = input.trim().to_string();
    if value.is_empty() {
        return Err("A URL is required.".into());
    }
    if !value.contains("://") {
        value = format!("https://{value}");
    }
    let parsed =
        url::Url::parse(&value).map_err(|_| format!("Invalid URL: {input}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Only http and https URLs can be saved.".into());
    }
    if parsed.host_str().is_none() {
        return Err("The URL has no host.".into());
    }
    Ok(value)
}

fn fallback_article(bookmark: &Bookmark) -> Article {
    let mut blocks = Vec::new();
    if let Some(description) = bookmark
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        blocks.push(("Description", description.to_string()));
    }
    if let Some(note) = bookmark
        .note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        blocks.push(("Your note", note.to_string()));
    }
    if blocks.is_empty() {
        blocks.push((
            "",
            "Readeck has no article text for this bookmark yet.\n\n\
             The extraction may still be running, or this entry may only be a link. \
             Press o to open the original page in your browser, or r to try fetching the article again."
                .to_string(),
        ));
    }
    Article::from_text_blocks(blocks)
}

async fn run_oauth_login(
    client: ReadeckClient,
    seq: u64,
    cancel: Arc<AtomicBool>,
    tx: UnboundedSender<Event>,
) {
    let client_id = match client.oauth_register().await {
        Ok(client_id) => client_id,
        Err(err) => {
            let _ = tx.send(Event::api(ApiEvent::OAuthDevice {
                seq,
                result: Err(err),
            }));
            return;
        }
    };

    let device = match client.oauth_device(&client_id).await {
        Ok(device) => device,
        Err(err) => {
            let _ = tx.send(Event::api(ApiEvent::OAuthDevice {
                seq,
                result: Err(err),
            }));
            return;
        }
    };

    let poll_interval = device.interval.max(1);
    let expires_in = device.expires_in.max(60);
    let started = Instant::now();
    if tx
        .send(Event::api(ApiEvent::OAuthDevice {
            seq,
            result: Ok(device.clone()),
        }))
        .is_err()
    {
        return;
    }

    let mut wait = poll_interval;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if started.elapsed().as_secs() >= expires_in {
            let _ = tx.send(Event::api(ApiEvent::OAuthToken {
                seq,
                result: Err(ApiError::OAuth {
                    code: "expired_token".into(),
                    message: "The device authorization code expired.".into(),
                }),
            }));
            return;
        }

        tokio::time::sleep(Duration::from_secs(wait)).await;
        if cancel.load(Ordering::Relaxed) {
            return;
        }

        let result =
            match client.oauth_token(&client_id, &device.device_code).await {
                Ok(OAuthPoll::Pending) => continue,
                Ok(OAuthPoll::SlowDown) => {
                    wait = (wait + 5).min(60);
                    continue;
                }
                Ok(OAuthPoll::Token(token)) => Ok(token),
                Ok(OAuthPoll::Denied(message)) => Err(ApiError::OAuth {
                    code: "access_denied".into(),
                    message,
                }),
                Ok(OAuthPoll::Expired(message)) => Err(ApiError::OAuth {
                    code: "expired_token".into(),
                    message,
                }),
                Err(err) => Err(err),
            };
        let _ = tx.send(Event::api(ApiEvent::OAuthToken { seq, result }));
        return;
    }
}

fn article_error(err: &ApiError) -> String {
    if err.is_no_article() {
        "No article is available for this bookmark yet. Press r to retry."
            .into()
    } else {
        err.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            normalize_url("example.com/article").unwrap(),
            "https://example.com/article"
        );
        assert_eq!(
            normalize_url("http://example.com").unwrap(),
            "http://example.com"
        );
        assert!(normalize_url("   ").is_err());
        assert!(normalize_url("ftp://example.com").is_err());
        assert!(normalize_url("https://").is_err());
    }

    #[test]
    fn parses_label_lists() {
        assert_eq!(
            parse_labels(" rust, tui ,, rust, read-later "),
            vec!["rust", "tui", "read-later"]
        );
        assert!(parse_labels("  , ").is_empty());
    }

    #[test]
    fn filter_membership() {
        let mut bookmark = Bookmark {
            id: "1".into(),
            read_progress: 0,
            ..Default::default()
        };
        assert!(keeps_filter(&Filter::Unread, &bookmark));
        assert!(!keeps_filter(&Filter::Reading, &bookmark));

        bookmark.read_progress = 50;
        assert!(keeps_filter(&Filter::Reading, &bookmark));
        assert!(keeps_filter(&Filter::Unread, &bookmark));

        bookmark.read_progress = 100;
        assert!(keeps_filter(&Filter::Unread, &bookmark));

        bookmark.labels = vec!["rust".into()];
        assert!(keeps_filter(&Filter::Label("rust".into()), &bookmark));
        assert!(!keeps_filter(&Filter::Label("tui".into()), &bookmark));

        bookmark.is_archived = true;
        assert!(keeps_filter(&Filter::Label("rust".into()), &bookmark));
        assert!(!keeps_filter(&Filter::Unread, &bookmark));
        assert!(keeps_filter(&Filter::Archived, &bookmark));
    }

    #[test]
    fn fallback_article_uses_description() {
        let bookmark = Bookmark {
            description: Some("A description".into()),
            note: Some("A note".into()),
            ..Default::default()
        };
        let article = fallback_article(&bookmark);
        assert!(
            article
                .blocks
                .iter()
                .any(|b| b.text.contains("A description"))
        );
        assert!(article.blocks.iter().any(|b| b.text.contains("A note")));
    }

    #[test]
    fn reader_progress_tracks_position() {
        let bookmark = Bookmark {
            has_article: true,
            ..Default::default()
        };
        let mut reader = Reader::new(bookmark, true, 0, true);
        reader.viewport = 10;
        let blocks = (0..40)
            .map(|i| {
                (
                    "",
                    format!("Paragraph number {i} with enough text to wrap a little."),
                )
            })
            .collect();
        reader.set_article(Article::from_text_blocks(blocks));

        assert!(reader.lines.len() > reader.viewport);
        assert_eq!(reader.half_page_amount(), 5);
        assert_eq!(reader.progress(), 0);
        assert!(!reader.scrolled);

        let max = reader.max_scroll();
        reader.jump_to(max);
        assert_eq!(reader.progress(), 100);
        assert!(reader.scrolled);

        reader.scroll_to(0);
        assert_eq!(reader.progress(), 0);
    }

    #[test]
    fn escape_clears_article_search_before_closing_reader() {
        let mut app = test_app_with_bookmarks(1);
        app.bookmarks[0].has_article = false;
        app.open_reader();
        if let Some(reader) = &mut app.reader {
            reader.viewport = 1;
            reader.set_article(Article::from_text_blocks(vec![
                ("", "alpha first".into()),
                ("", "alpha second".into()),
            ]));
            assert_eq!(reader.search("alpha"), 2);
        }

        app.handle_reader_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.reader.is_some());
        assert!(app.reader.as_ref().unwrap().search_query.is_none());

        app.handle_reader_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.reader.is_none());

        app.open_reader();
        if let Some(reader) = &mut app.reader {
            assert_eq!(reader.search("no-such-text"), 0);
        }
        app.handle_reader_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.reader.is_none());
    }

    #[test]
    fn article_search_finds_and_navigates_matches() {
        let bookmark = Bookmark {
            has_article: true,
            ..Default::default()
        };
        let mut reader = Reader::new(bookmark, true, 0, true);
        reader.viewport = 1;
        reader.set_article(Article::from_text_blocks(vec![
            ("", "alpha first".into()),
            ("", "beta".into()),
            ("", "Alpha second".into()),
        ]));

        assert_eq!(reader.search("alpha"), 2);
        assert_eq!(reader.scroll, 0);

        reader.next_search_match(true);
        assert_eq!(reader.scroll, 4);

        reader.next_search_match(true);
        assert_eq!(reader.scroll, 4);

        reader.next_search_match(false);
        assert_eq!(reader.scroll, 0);

        reader.next_search_match(true);
        assert_eq!(reader.scroll, 4);
        reader.next_search_match(false);
        assert_eq!(reader.scroll, 0);

        assert_eq!(reader.search(""), 0);
        assert!(reader.search_query.is_none());
    }

    #[test]
    fn space_scrolls_to_the_next_paragraph() {
        let bookmark = Bookmark {
            has_article: true,
            ..Default::default()
        };
        let mut reader = Reader::new(bookmark, true, 0, true);
        reader.viewport = 2;
        reader.set_article(Article::from_text_blocks(vec![
            ("", "First paragraph".into()),
            ("", "Second paragraph".into()),
            ("", "Third paragraph".into()),
        ]));

        reader.scroll_to_next_paragraph();
        assert_eq!(reader.scroll, 2);
        assert!(reader.lines[2].kind.is_some());
        assert!(reader.lines[1].kind.is_none());
        assert!(reader.scrolled);
    }

    #[test]
    fn reader_restores_saved_progress() {
        let bookmark = Bookmark {
            has_article: true,
            read_progress: 50,
            ..Default::default()
        };
        let mut reader = Reader::new(bookmark, true, 0, true);
        reader.viewport = 10;
        let blocks = (0..40)
            .map(|i| {
                (
                    "",
                    format!("Paragraph number {i} with enough text to wrap a little."),
                )
            })
            .collect();
        reader.set_article(Article::from_text_blocks(blocks));

        let max = reader.max_scroll();
        assert!(max > 0);
        let expected = (max as f32 * 0.5).round() as usize;
        assert_eq!(reader.scroll, expected);
        assert!((49..=51).contains(&reader.progress()));
        assert!(!reader.scrolled);

        reader.scroll_by(1);
        assert!(reader.scrolled);
    }

    fn test_app_with_bookmarks(count: usize) -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app =
            App::new(Config::default(), PathBuf::from("/tmp/eckly-test"), tx);
        app.screen = Screen::Main;
        app.filter = Filter::All;
        app.bookmarks = (0..count)
            .map(|i| Bookmark {
                id: format!("bookmark-{i}"),
                title: Some(format!("Bookmark {i}")),
                ..Default::default()
            })
            .collect();
        app.total = count;
        app.list_state.select(Some(0));
        app
    }

    #[test]
    fn reader_respects_maximum_text_width() {
        let bookmark = Bookmark {
            has_article: true,
            ..Default::default()
        };
        let mut reader = Reader::new(bookmark, true, 40, true);
        reader.viewport = 10;
        reader.set_article(Article::from_text_blocks(vec![(
            "",
            "A long paragraph that would normally use the full terminal width. ".repeat(10),
        )]));
        reader.reflow(100);

        assert_eq!(reader.width, 40);
        assert!(
            reader
                .lines
                .iter()
                .all(|line| line.text.chars().count() <= 40)
        );
    }

    #[test]
    fn reading_view_can_delete_the_bookmark() {
        let mut app = test_app_with_bookmarks(1);
        app.bookmarks[0].has_article = false;
        app.open_reader();

        app.handle_reader_key(KeyEvent::new(
            KeyCode::Char('D'),
            KeyModifiers::NONE,
        ));

        match app.modal.as_ref().unwrap() {
            Modal::Confirm(confirm) => match &confirm.action {
                ConfirmAction::DeleteBookmarks { ids } => {
                    assert_eq!(ids, &vec!["bookmark-0"]);
                }
            },
            _ => panic!("expected a confirmation modal"),
        }
    }

    #[test]
    fn text_alignment_can_be_toggled_in_the_reader() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app =
            App::new(Config::default(), PathBuf::from("/tmp/eckly-test"), tx);
        app.reader = Some(Reader::new(Bookmark::default(), true, 80, true));

        app.toggle_reader_alignment();
        assert!(!app.reader.as_ref().unwrap().center_text);
        assert_eq!(app.config.text_align, "left");

        app.toggle_reader_alignment();
        assert!(app.reader.as_ref().unwrap().center_text);
        assert_eq!(app.config.text_align, "center");
    }

    #[test]
    fn images_can_be_toggled_in_the_reader() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let config = Config {
            images: false,
            ..Default::default()
        };
        let mut app = App::new(config, PathBuf::from("/tmp/eckly-test"), tx);
        assert!(!app.images_enabled);

        let mut reader = Reader::new(Bookmark::default(), false, 0, true);
        reader.set_article(parse_article(
            r#"<p>Text</p><img src="/image.png" alt="Image">"#,
        ));
        app.reader = Some(reader);

        app.toggle_reader_images();
        assert!(app.images_enabled);
        assert!(app.reader.as_ref().unwrap().show_images);

        app.toggle_reader_images();
        assert!(!app.images_enabled);
        assert!(!app.reader.as_ref().unwrap().show_images);
    }

    #[test]
    fn opening_reader_keeps_selection() {
        let mut app = test_app_with_bookmarks(2);
        app.bookmarks[0].has_article = false;
        app.selected_ids.insert("bookmark-0".into());
        app.selected_ids.insert("bookmark-1".into());

        app.open_reader();

        assert!(app.reader.is_some());
        assert_eq!(app.selected_ids.len(), 2);
    }

    #[test]
    fn library_search_filters_labels_and_applies() {
        let mut app = test_app_with_bookmarks(1);
        app.focus = Focus::Sidebar;
        app.labels = vec![
            Label {
                name: "rust".into(),
                count: 1,
            },
            Label {
                name: "rust-lang".into(),
                count: 1,
            },
            Label {
                name: "tui".into(),
                count: 1,
            },
        ];

        app.handle_sidebar_key(KeyEvent::new(
            KeyCode::Char('/'),
            KeyModifiers::NONE,
        ));
        assert!(app.sidebar_search_active);
        for ch in "rust".chars() {
            app.handle_sidebar_key(KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::NONE,
            ));
        }

        let texts: Vec<&str> = app
            .sidebar_items
            .iter()
            .map(|item| item.text.as_str())
            .collect();
        assert!(texts.contains(&"# rust"));
        assert!(texts.contains(&"# rust-lang"));
        assert!(!texts.contains(&"# tui"));

        app.handle_sidebar_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ));
        assert_eq!(app.filter, Filter::Label("rust".into()));
        assert!(!app.sidebar_search_active);
        assert!(app.sidebar_search.value.is_empty());
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn sidebar_page_keys_move_selection() {
        let mut app = test_app_with_bookmarks(1);
        app.focus = Focus::Sidebar;
        app.labels = (0..8)
            .map(|i| Label {
                name: format!("label-{i}"),
                count: 1,
            })
            .collect();
        app.sidebar_viewport = 5;
        app.rebuild_sidebar();
        app.sidebar_state.select(Some(0));

        app.handle_sidebar_key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        ));
        let selected = app.sidebar_state.selected().unwrap();
        assert!(selected > 0);
        assert!(!app.sidebar_items[selected].header);

        app.handle_sidebar_key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ));
        let after = app.sidebar_state.selected().unwrap();
        assert!(after != selected);
        assert!(!app.sidebar_items[after].header);
    }

    #[test]
    fn list_escape_clears_search_then_selection_then_focuses_library() {
        let mut app = test_app_with_bookmarks(2);
        app.search = "rust".into();
        app.selected_ids.insert("bookmark-0".into());

        app.handle_list_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.search.is_empty());
        assert!(app.selected_ids.is_empty());
        assert_eq!(app.focus, Focus::List);

        app.selected_ids.insert("bookmark-0".into());
        app.handle_list_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.selected_ids.is_empty());
        assert_eq!(app.focus, Focus::List);

        app.handle_list_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn unread_is_the_default_and_first_library_section() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app =
            App::new(Config::default(), PathBuf::from("/tmp/eckly-test"), tx);
        assert_eq!(app.filter, Filter::Unread);

        app.rebuild_sidebar();
        let filters: Vec<Filter> = app
            .sidebar_items
            .iter()
            .filter(|item| !item.header)
            .take(5)
            .filter_map(|item| item.filter.clone())
            .collect();
        assert_eq!(
            filters,
            vec![
                Filter::Unread,
                Filter::Reading,
                Filter::Marked,
                Filter::Archived,
                Filter::All,
            ]
        );
    }

    #[test]
    fn ctrl_a_selects_all_loaded_bookmarks() {
        let mut app = test_app_with_bookmarks(3);

        app.handle_list_key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL,
        ));

        assert_eq!(app.selected_ids.len(), 3);
        assert!(app.selected_ids.contains("bookmark-0"));
        assert!(app.selected_ids.contains("bookmark-1"));
        assert!(app.selected_ids.contains("bookmark-2"));
    }

    #[test]
    fn space_toggles_bookmark_selection() {
        let mut app = test_app_with_bookmarks(2);

        app.handle_list_key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
        ));
        assert!(app.selected_ids.contains("bookmark-0"));

        app.handle_list_key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
        ));
        assert!(app.selected_ids.is_empty());
    }

    #[test]
    fn actions_target_selection_or_highlight() {
        let mut app = test_app_with_bookmarks(3);
        app.list_state.select(Some(1));
        app.selected_ids.insert("bookmark-0".into());
        app.selected_ids.insert("bookmark-2".into());

        let ids: Vec<String> = app
            .action_targets()
            .into_iter()
            .map(|bookmark| bookmark.id)
            .collect();
        assert_eq!(ids, vec!["bookmark-0", "bookmark-2"]);

        app.selected_ids.clear();
        let ids: Vec<String> = app
            .action_targets()
            .into_iter()
            .map(|bookmark| bookmark.id)
            .collect();
        assert_eq!(ids, vec!["bookmark-1"]);
    }

    #[test]
    fn bulk_update_applies_patch_and_keeps_selection() {
        let mut app = test_app_with_bookmarks(3);
        app.selected_ids.insert("bookmark-0".into());
        app.selected_ids.insert("bookmark-1".into());
        let ids = vec!["bookmark-0".to_string(), "bookmark-1".to_string()];

        app.handle_api(ApiEvent::BulkUpdated {
            ids,
            patch: BookmarkPatch {
                read_progress: Some(100),
                ..Default::default()
            },
            message: "Marked 2 bookmarks as read".into(),
            quiet: true,
            result: Ok(2),
        });

        assert_eq!(app.bookmarks[0].read_progress, 100);
        assert_eq!(app.bookmarks[1].read_progress, 100);
        assert_eq!(app.bookmarks[2].read_progress, 0);
        assert!(app.selected_ids.contains("bookmark-0"));
        assert!(app.selected_ids.contains("bookmark-1"));
    }

    #[test]
    fn delete_confirmation_targets_the_selection() {
        let mut app = test_app_with_bookmarks(3);
        app.list_state.select(Some(1));
        app.selected_ids.insert("bookmark-0".into());
        app.selected_ids.insert("bookmark-2".into());

        app.confirm_delete();
        match app.modal.as_ref().unwrap() {
            Modal::Confirm(confirm) => match &confirm.action {
                ConfirmAction::DeleteBookmarks { ids } => {
                    assert_eq!(ids, &vec!["bookmark-0", "bookmark-2"]);
                }
            },
            _ => panic!("expected a confirmation modal"),
        }
    }

    #[test]
    fn bulk_delete_removes_all_selected_bookmarks() {
        let mut app = test_app_with_bookmarks(3);
        app.selected_ids.insert("bookmark-0".into());
        app.selected_ids.insert("bookmark-2".into());
        let ids = vec!["bookmark-0".to_string(), "bookmark-2".to_string()];

        app.handle_api(ApiEvent::BulkDeleted { ids, result: Ok(2) });

        assert_eq!(app.bookmarks.len(), 1);
        assert_eq!(app.bookmarks[0].id, "bookmark-1");
        assert_eq!(app.total, 1);
        assert!(app.selected_ids.is_empty());
    }

    #[test]
    fn bulk_label_editor_uses_shared_labels() {
        let mut app = test_app_with_bookmarks(2);
        for bookmark in &mut app.bookmarks {
            bookmark.labels = vec!["rust".into(), "tui".into()];
        }
        app.selected_ids.insert("bookmark-0".into());
        app.selected_ids.insert("bookmark-1".into());

        app.edit_labels();
        match app.modal.as_ref().unwrap() {
            Modal::Input(input) => {
                assert_eq!(input.input.value, "rust, tui");
                assert!(matches!(input.kind, InputKind::EditLabels { .. }));
            }
            _ => panic!("expected an input modal"),
        }

        app.bookmarks[1].labels = vec!["different".into()];
        app.modal = None;
        app.edit_labels();
        match app.modal.as_ref().unwrap() {
            Modal::Input(input) => assert!(input.input.value.is_empty()),
            _ => panic!("expected an input modal"),
        }
    }

    #[test]
    fn tab_cycles_label_completions() {
        let mut app = test_app_with_bookmarks(1);
        app.labels = vec![
            Label {
                name: "read-later".into(),
                count: 1,
            },
            Label {
                name: "rust".into(),
                count: 1,
            },
            Label {
                name: "rust-lang".into(),
                count: 1,
            },
            Label {
                name: "tui".into(),
                count: 1,
            },
        ];
        app.modal = Some(Modal::Input(InputModal::new(
            "Edit labels",
            "",
            TextInput::new("ru"),
            InputKind::EditLabels {
                ids: vec!["bookmark-0".into()],
            },
        )));

        app.handle_modal_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(label_modal_value(&app), "rust");
        app.handle_modal_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(label_modal_value(&app), "rust-lang");
        app.handle_modal_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(label_modal_value(&app), "rust");

        assert_eq!(
            label_suggestions(&app.labels, "tui, re", 8),
            vec!["read-later"]
        );
    }

    fn label_modal_value(app: &App) -> String {
        match app.modal.as_ref().unwrap() {
            Modal::Input(input) => input.input.value.clone(),
            _ => panic!("expected an input modal"),
        }
    }

    #[test]
    fn ctrl_w_deletes_a_word_in_search_and_label_dialogs() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app =
            App::new(Config::default(), PathBuf::from("/tmp/eckly-test"), tx);

        for kind in [
            InputKind::Search,
            InputKind::EditLabels {
                ids: vec!["bookmark-0".into()],
            },
        ] {
            app.modal = Some(Modal::Input(InputModal::new(
                "Edit",
                "",
                TextInput::new("rust tui"),
                kind,
            )));

            app.handle_modal_key(KeyEvent::new(
                KeyCode::Char('w'),
                KeyModifiers::CONTROL,
            ));

            match app.modal.as_ref().unwrap() {
                Modal::Input(input) => assert_eq!(input.input.value, "rust "),
                _ => panic!("expected an input modal"),
            }
        }
    }

    #[test]
    fn ctrl_d_and_ctrl_u_scroll_list_half_a_page() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app =
            App::new(Config::default(), PathBuf::from("/tmp/eckly-test"), tx);
        app.bookmarks = (0..20)
            .map(|i| Bookmark {
                id: format!("bookmark-{i}"),
                ..Default::default()
            })
            .collect();
        app.total = app.bookmarks.len();
        app.list_viewport = 10;
        app.list_state.select(Some(0));

        app.handle_list_key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ));
        assert_eq!(app.list_state.selected(), Some(5));

        app.handle_list_key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        ));
        assert_eq!(app.list_state.selected(), Some(0));

        app.handle_list_key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        ));
        assert_eq!(app.list_state.selected(), Some(10));
    }

    #[test]
    fn ctrl_d_and_ctrl_u_scroll_half_a_page() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app =
            App::new(Config::default(), PathBuf::from("/tmp/eckly-test"), tx);

        let bookmark = Bookmark {
            has_article: true,
            ..Default::default()
        };
        let mut reader = Reader::new(bookmark, true, 0, true);
        reader.viewport = 10;
        let blocks = (0..40)
            .map(|i| {
                ("", format!("Paragraph number {i}, long enough to wrap."))
            })
            .collect();
        reader.set_article(Article::from_text_blocks(blocks));
        app.reader = Some(reader);

        app.handle_reader_key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ));
        assert_eq!(app.reader.as_ref().unwrap().scroll, 5);

        app.handle_reader_key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        ));
        assert_eq!(app.reader.as_ref().unwrap().scroll, 0);

        app.handle_reader_key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        ));
        assert_eq!(app.reader.as_ref().unwrap().scroll, 0);
    }
}
