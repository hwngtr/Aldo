//! TUI state, and the pure decisions made about it.
//!
//! Nothing here draws or touches the terminal. The key map, the selection
//! arithmetic and the message handling are therefore unit-tested without a TTY,
//! and the render in `ui` is the only thing that needs a terminal.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use aldo_core::candidate::{Candidate, CandidateFile};
use aldo_core::ids::CandidateId;
use aldo_store::repo::CatalogRow;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::actions::download::{DownloadEvent, DownloadPlan, DownloadReport, Prepared};
use crate::actions::reports::{BudgetLine, SessionLine};
use crate::actions::search::{MAX_SOULSEEK_RESULTS, SearchPhase, SearchRun, SoulSeekReport};
use crate::credentials::Credentials;
use crate::format::basename;

/// Which full-screen view is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The first-run credential form, shown until an account is saved.
    Setup,
    Home,
    Results,
    Download,
    Budgets,
    Sessions,
    Help,
}

/// Which result list the keyboard is acting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sources,
    Discogs,
}

impl Focus {
    fn flipped(self) -> Self {
        match self {
            Self::Sources => Self::Discogs,
            Self::Discogs => Self::Sources,
        }
    }
}

/// How loud a transient message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

/// A message shown for a few seconds, then forgotten.
#[derive(Debug, Clone)]
pub struct Notice {
    pub text: String,
    pub level: Level,
    until: Instant,
}

/// A single-line editor, with the caret as a character index.
///
/// It has one selection, "everything", because select-all is the only way to
/// select. A range would be more machinery than the feature needs, and the
/// moment shift-arrows arrive it will be the wrong shape anyway.
#[derive(Debug, Default, Clone)]
pub struct Input {
    text: String,
    cursor: usize,
    selected: bool,
    secret: bool,
}

impl Input {
    /// A field whose contents are hidden, for a password or a token.
    #[must_use]
    pub fn secret() -> Self {
        Self {
            secret: true,
            ..Self::default()
        }
    }

    /// Whether a renderer should draw the contents as marks, not characters.
    #[must_use]
    pub fn is_secret(&self) -> bool {
        self.secret
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Whether the whole line is selected, so a renderer can show it as a block
    /// of highlight rather than text with a caret in it.
    #[must_use]
    pub fn all_selected(&self) -> bool {
        self.selected
    }

    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.chars().count();
        self.selected = false;
    }

    pub fn select_all(&mut self) {
        if !self.text.is_empty() {
            self.selected = true;
            self.cursor = self.text.chars().count();
        }
    }

    /// The caret's byte offset, which is what `String` editing needs.
    fn byte_at(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map_or(self.text.len(), |(offset, _)| offset)
    }

    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.selected = false;
    }

    pub fn insert(&mut self, ch: char) {
        // Typing over a selection replaces it, the way a text field does.
        if self.selected {
            self.clear();
        }
        let at = self.byte_at(self.cursor);
        self.text.insert(at, ch);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.selected {
            self.clear();
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let end = self.byte_at(self.cursor);
        let start = self.byte_at(self.cursor - 1);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        if self.selected {
            self.clear();
            return;
        }
        if self.cursor >= self.text.chars().count() {
            return;
        }
        let start = self.byte_at(self.cursor);
        let end = self.byte_at(self.cursor + 1);
        self.text.replace_range(start..end, "");
    }

    pub fn left(&mut self) {
        self.selected = false;
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.selected = false;
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    pub fn home(&mut self) {
        self.selected = false;
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.selected = false;
        self.cursor = self.text.chars().count();
    }

    /// The caret as a character index, for a renderer that needs to split the
    /// text it is about to draw.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }
}

/// Which credential field the first-run screen is editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupField {
    Username,
    Password,
    Token,
}

impl SetupField {
    fn next(self) -> Self {
        match self {
            Self::Username => Self::Password,
            Self::Password => Self::Token,
            Self::Token => Self::Username,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Username => Self::Token,
            Self::Password => Self::Username,
            Self::Token => Self::Password,
        }
    }
}

/// The first-run credential form.
#[derive(Debug)]
pub struct Setup {
    pub username: Input,
    pub password: Input,
    pub token: Input,
    pub field: SetupField,
}

impl Setup {
    #[must_use]
    pub fn new() -> Self {
        Self {
            username: Input::default(),
            password: Input::secret(),
            token: Input::secret(),
            field: SetupField::Username,
        }
    }

    fn focus(&mut self, previous: bool) {
        self.field = if previous {
            self.field.previous()
        } else {
            self.field.next()
        };
    }

    fn input_mut(&mut self) -> &mut Input {
        match self.field {
            SetupField::Username => &mut self.username,
            SetupField::Password => &mut self.password,
            SetupField::Token => &mut self.token,
        }
    }

    /// What the form still needs before it can be saved.
    #[must_use]
    pub fn missing(&self) -> Option<&'static str> {
        if self.username.is_blank() {
            Some("a SoulSeek username")
        } else if self.password.is_blank() {
            Some("a SoulSeek password")
        } else {
            None
        }
    }

    /// The credentials as typed. The Discogs token is optional.
    #[must_use]
    pub fn credentials(&self) -> Credentials {
        Credentials {
            slsk_username: Some(self.username.text().trim().to_owned()),
            slsk_password: Some(self.password.text().trim().to_owned()),
            discogs_token: non_blank(self.token.text()),
        }
    }
}

impl Default for Setup {
    fn default() -> Self {
        Self::new()
    }
}

/// Trims a field, treating whitespace-only text as absent.
fn non_blank(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The files in one source's folder, shown as a panel of its own.
#[derive(Debug, Clone)]
pub struct FilesView {
    pub peer: String,
    pub folder: String,
    pub files: Vec<CandidateFile>,
    pub selected: usize,
}

/// Where one file in the download list stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Pending,
    Active,
    Done,
    Skipped,
}

/// One row of the download list.
#[derive(Debug, Clone)]
pub struct FileLine {
    pub name: String,
    pub size: u64,
    pub state: FileState,
}

/// How far the whole download has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStage {
    Connecting,
    Running,
    Done,
    Failed,
}

/// The file currently being transferred, so its own bar can be drawn.
#[derive(Debug, Clone)]
pub struct CurrentFile {
    pub index: usize,
    pub name: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub speed_bps: u32,
    pub queued: bool,
}

/// One download, as it happens.
#[derive(Debug, Clone)]
pub struct DownloadView {
    pub title: String,
    pub peer: String,
    pub files: Vec<FileLine>,
    pub total_bytes: u64,
    /// Bytes of the files that have finished.
    pub finished_bytes: u64,
    pub current: Option<CurrentFile>,
    pub stage: DownloadStage,
    pub message: String,
    pub finished: Option<DownloadReport>,
    pub selected: usize,
}

impl DownloadView {
    #[must_use]
    pub fn new(plan: &DownloadPlan) -> Self {
        Self {
            title: plan.title(),
            peer: plan.peer().unwrap_or("unknown").to_owned(),
            files: plan
                .files
                .iter()
                .map(|file| FileLine {
                    name: basename(&file.path),
                    size: file.size.as_u64(),
                    state: FileState::Pending,
                })
                .collect(),
            total_bytes: plan.total_bytes.as_u64(),
            finished_bytes: 0,
            current: None,
            stage: DownloadStage::Connecting,
            message: format!("connecting as {}…", plan.peer().unwrap_or("peer")),
            finished: None,
            selected: 0,
        }
    }

    /// Bytes done so far, counting the part of the active file.
    #[must_use]
    pub fn done_bytes(&self) -> u64 {
        let partial = self.current.as_ref().map_or(0, |file| file.bytes_done);
        self.finished_bytes
            .saturating_add(partial)
            .min(self.total_bytes)
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(
            self.stage,
            DownloadStage::Connecting | DownloadStage::Running
        )
    }

    /// Folds one event into the view.
    pub fn apply(&mut self, event: &DownloadEvent) {
        match event {
            DownloadEvent::Connecting { username } => {
                self.stage = DownloadStage::Connecting;
                self.message = format!("connecting as {username}…");
            }
            DownloadEvent::FileStarted {
                index,
                name,
                size,
                total,
            } => {
                self.stage = DownloadStage::Running;
                self.set_state(*index, FileState::Active);
                self.current = Some(CurrentFile {
                    index: *index,
                    name: name.clone(),
                    bytes_done: 0,
                    bytes_total: size.as_u64(),
                    speed_bps: 0,
                    queued: false,
                });
                self.message = format!("file {index} of {total}");
                self.selected = index.saturating_sub(1);
            }
            DownloadEvent::FileSkipped { index, name, .. } => {
                self.set_state(*index, FileState::Skipped);
                self.finish_file(*index);
                self.message = format!("{name} was already downloaded");
            }
            DownloadEvent::Progress {
                bytes_done,
                bytes_total,
                speed_bps,
                queued,
                ..
            } => {
                if let Some(current) = self.current.as_mut() {
                    current.bytes_done = *bytes_done;
                    current.bytes_total = *bytes_total;
                    current.speed_bps = *speed_bps;
                    current.queued = *queued;
                }
            }
            DownloadEvent::FileDone { index, .. } => {
                self.set_state(*index, FileState::Done);
                self.finish_file(*index);
                self.current = None;
            }
            DownloadEvent::Finished(report) => {
                self.stage = DownloadStage::Done;
                self.finished_bytes = self.total_bytes;
                self.current = None;
                self.message = format!("saved to {}", report.target_dir.display());
                self.finished = Some(report.clone());
            }
        }
    }

    fn set_state(&mut self, index: usize, state: FileState) {
        if let Some(line) = self.files.get_mut(index.saturating_sub(1)) {
            line.state = state;
        }
    }

    fn finish_file(&mut self, index: usize) {
        if let Some(line) = self.files.get(index.saturating_sub(1)) {
            self.finished_bytes = self.finished_bytes.saturating_add(line.size);
        }
    }
}

/// A confirmation before a download touches the filesystem.
#[derive(Debug, Clone)]
pub struct Confirm {
    pub prepared: Prepared,
    pub library_root: PathBuf,
}

/// What the interface knows about the installer's setup, shown on the home
/// screen so a missing token is obvious before a search is attempted.
#[derive(Debug, Clone)]
pub struct ConfigSummary {
    pub library_root: PathBuf,
    pub discogs: bool,
    pub server: bool,
}

/// Something the interface wants done in the background.
///
/// Kept as a plain value so the key map is tested by asserting on intents,
/// rather than on a spawned task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Save the credentials typed on the first-run screen.
    SaveCredentials(Box<Credentials>),
    Search(String),
    /// Resolve a source into a plan and its tags before asking to download it.
    PrepareDownload(CandidateId),
    /// Record which pressing to tag with, then resolve it for confirmation.
    ChooseRelease {
        candidate_id: CandidateId,
        release_id: u32,
    },
    /// Run an already-confirmed download.
    Download(Box<Prepared>),
    ReloadBudgets,
    ReloadSessions,
    Quit,
}

/// Everything the event loop and the background tasks send to the app.
#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    /// A resize or similar: draw again without changing anything.
    Redraw,
    Phase(SearchPhase),
    /// Boxed because a search result is much larger than a keystroke.
    Search(Box<Result<SearchRun, String>>),
    /// Boxed for the same reason: a plan carries every file.
    Prepared(Box<Result<Confirm, String>>),
    /// The first-run save finished. Carries the credentials so the settings can
    /// adopt them without re-reading the file.
    SetupSaved(Box<Result<Credentials, String>>),
    Download(DownloadEvent),
    DownloadResult(Result<(), String>),
    Budgets(Result<Vec<BudgetLine>, String>),
    Sessions(Result<Vec<SessionLine>, String>),
}

/// The whole interface state.
#[derive(Debug)]
pub struct App {
    pub config: ConfigSummary,
    pub screen: Screen,
    /// The screen to return to when help closes.
    help_return: Screen,
    pub input: Input,
    /// True while the search prompt is open and swallowing keystrokes.
    pub input_open: bool,
    pub phase: Option<SearchPhase>,
    pub spinner: u64,
    pub search: Option<SearchRun>,
    pub error: Option<String>,
    pub notice: Option<Notice>,
    /// The credential form, used while [`Screen::Setup`] is showing.
    pub setup: Setup,
    pub focus: Focus,
    pub source_row: usize,
    pub discogs_row: usize,
    pub budget_row: usize,
    pub session_row: usize,
    pub files: Option<FilesView>,
    pub budgets: Vec<BudgetLine>,
    pub sessions: Vec<SessionLine>,
    /// The download in progress or just finished, if any.
    pub download: Option<DownloadView>,
    /// The confirmation modal, if a download is being asked about.
    pub confirm: Option<Confirm>,
    /// A query handed in on the command line, to be run as soon as the loop
    /// starts. `None` once it has been taken.
    pending_search: Option<String>,
    dirty: bool,
}

impl App {
    #[must_use]
    pub fn new(config: ConfigSummary, prefilled: Option<String>) -> Self {
        let mut input = Input::default();
        let mut pending_search = None;

        if let Some(query) = prefilled {
            let query = query.trim().to_owned();
            if query.is_empty() {
                // An empty `-q` is the same as no `-q`; open the box instead.
                input = Input::default();
            } else {
                input.set(query.clone());
                pending_search = Some(query);
            }
        }

        // Without an account there is nothing to search, so the form comes
        // before anything else, including a query handed in on the command line.
        let needs_setup = !config.server;

        Self {
            config,
            screen: if needs_setup {
                Screen::Setup
            } else if pending_search.is_some() {
                Screen::Results
            } else {
                Screen::Home
            },
            help_return: Screen::Home,
            input,
            input_open: false,
            phase: if !needs_setup && pending_search.is_some() {
                Some(SearchPhase::Discogs)
            } else {
                None
            },
            spinner: 0,
            search: None,
            error: None,
            notice: None,
            setup: Setup::new(),
            focus: Focus::Sources,
            source_row: 0,
            discogs_row: 0,
            budget_row: 0,
            session_row: 0,
            files: None,
            budgets: Vec::new(),
            sessions: Vec::new(),
            download: None,
            confirm: None,
            pending_search,
            dirty: true,
        }
    }

    /// The query given on the command line, taken exactly once.
    pub fn take_pending_search(&mut self) -> Option<String> {
        self.pending_search.take()
    }

    /// The SoulSeek folders to offer, capped to what is worth scanning.
    #[must_use]
    pub fn sources(&self) -> &[Candidate] {
        match self.search.as_ref().map(|run| &run.soulseek) {
            Some(SoulSeekReport::Answered(found)) => {
                let shown = found.candidates.len().min(MAX_SOULSEEK_RESULTS);
                &found.candidates[..shown]
            }
            _ => &[],
        }
    }

    #[must_use]
    pub fn discogs(&self) -> &[CatalogRow] {
        self.search
            .as_ref()
            .map_or(&[], |run| run.discogs.matches.as_slice())
    }

    #[must_use]
    pub fn selected_source(&self) -> Option<&Candidate> {
        self.sources().get(self.source_row)
    }

    /// Why the SoulSeek half came back empty, when the reason is actionable.
    #[must_use]
    pub fn soulseek_hint(&self) -> Option<String> {
        match self.search.as_ref().map(|run| &run.soulseek) {
            Some(SoulSeekReport::Answered(found)) => found.hint(),
            _ => None,
        }
    }

    /// Whether the next frame must be drawn.
    #[must_use]
    pub fn needs_redraw(&self) -> bool {
        self.dirty || self.phase.is_some() || self.notice.is_some() || self.download_active()
    }

    /// Advances the spinner and expires a stale notice.
    pub fn on_tick(&mut self) {
        if self.phase.is_some() || self.download_active() {
            self.spinner = self.spinner.wrapping_add(1);
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| Instant::now() >= notice.until)
        {
            self.notice = None;
            self.dirty = true;
        }
    }

    pub fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Every state change redraws: a screen is never partially updated.
    fn touch(&mut self) {
        self.dirty = true;
    }

    fn notice_now(&mut self, level: Level, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            level,
            until: Instant::now() + Duration::from_secs(6),
        });
        self.touch();
    }

    fn go(&mut self, screen: Screen) {
        self.screen = screen;
        self.touch();
    }

    fn open_input(&mut self) {
        self.screen = Screen::Home;
        self.input_open = true;
        self.input.home();
        self.touch();
    }

    fn close_input(&mut self) {
        self.input_open = false;
        self.touch();
    }

    fn toggle_help(&mut self) {
        if self.screen == Screen::Help {
            self.screen = self.help_return;
        } else {
            self.help_return = self.screen;
            self.screen = Screen::Help;
        }
        self.touch();
    }

    /// Backs out one step: the prompt, then the file panel, then the download
    /// screen or the help overlay, then home.
    fn back(&mut self) {
        if self.input_open {
            self.close_input();
        } else if self.files.is_some() {
            self.files = None;
            self.touch();
        } else if self.screen == Screen::Download {
            // Leaving the screen does not stop the transfer; the events keep
            // folding in and the header keeps counting.
            self.go(Screen::Results);
        } else if self.screen == Screen::Help {
            self.screen = self.help_return;
            self.touch();
        } else if self.screen != Screen::Home {
            self.go(Screen::Home);
        }
    }

    /// Handles one message, returning the background work it asked for.
    pub fn update(&mut self, msg: Msg) -> Option<Intent> {
        match msg {
            Msg::Key(key) => self.on_key(key),
            Msg::Paste(text) => {
                self.paste(&text);
                None
            }
            Msg::Redraw => {
                self.touch();
                None
            }
            Msg::Phase(phase) => {
                self.phase = Some(phase);
                self.touch();
                None
            }
            Msg::Search(result) => {
                self.on_search(*result);
                None
            }
            Msg::Budgets(result) => {
                match result {
                    Ok(lines) => {
                        self.budgets = lines;
                        self.budget_row = 0;
                    }
                    Err(error) => self.notice_now(Level::Error, error),
                }
                self.touch();
                None
            }
            Msg::Prepared(result) => {
                match *result {
                    Ok(confirm) => self.confirm = Some(confirm),
                    Err(error) => self.notice_now(Level::Error, error),
                }
                self.touch();
                None
            }
            Msg::Download(event) => {
                if let Some(view) = self.download.as_mut() {
                    view.apply(&event);
                }
                self.touch();
                None
            }
            Msg::DownloadResult(result) => {
                match result {
                    Ok(()) => {
                        if let Some(view) = self.download.as_mut() {
                            view.stage = DownloadStage::Done;
                        }
                        self.notice_now(Level::Info, "download complete");
                    }
                    Err(error) => {
                        if let Some(view) = self.download.as_mut() {
                            view.stage = DownloadStage::Failed;
                            view.message.clone_from(&error);
                        }
                        self.notice_now(Level::Error, error);
                    }
                }
                self.touch();
                None
            }
            Msg::Sessions(result) => {
                match result {
                    Ok(rows) => {
                        self.sessions = rows;
                        self.session_row = 0;
                    }
                    Err(error) => self.notice_now(Level::Error, error),
                }
                self.touch();
                None
            }
            Msg::SetupSaved(result) => match *result {
                // The settings and the config chips were updated by the event
                // loop, which owns them. Here only the navigation is decided.
                Ok(_) => {
                    if let Some(query) = self.take_pending_search() {
                        self.screen = Screen::Results;
                        self.phase = Some(SearchPhase::Discogs);
                        self.error = None;
                        self.notice = None;
                        self.touch();
                        return Some(Intent::Search(query));
                    }
                    self.go(Screen::Home);
                    self.notice_now(Level::Info, "signed in");
                    None
                }
                Err(error) => {
                    self.notice_now(Level::Error, error);
                    None
                }
            },
        }
    }

    /// Inserts pasted text into whichever field is taking input.
    ///
    /// A pasted newline is a line break in the source text, not an Enter key: it
    /// must not run a half-typed search, nor submit a half-filled sign-in form.
    fn paste(&mut self, text: &str) {
        let text: Vec<char> = text
            .chars()
            .filter(|ch| *ch != '\n' && *ch != '\r')
            .collect();

        if self.screen == Screen::Setup {
            for ch in text {
                self.setup.input_mut().insert(ch);
            }
            self.touch();
        } else if self.input_open {
            for ch in text {
                self.input.insert(ch);
            }
            self.touch();
        }
    }

    fn on_search(&mut self, result: Result<SearchRun, String>) {
        self.phase = None;
        match result {
            Ok(run) => {
                // Land on whichever list actually has something.
                self.focus = if run.soulseek.sources().is_empty() {
                    Focus::Discogs
                } else {
                    Focus::Sources
                };
                self.source_row = 0;
                self.discogs_row = 0;
                let text = summary(&run);
                self.notice_now(Level::Info, text);
                self.search = Some(run);
                self.error = None;
            }
            Err(error) => {
                self.search = None;
                self.error = Some(error);
            }
        }
        self.touch();
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Intent> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Intent::Quit);
        }
        self.touch();

        // The first-run form owns every key until it is submitted: single-key
        // commands must not fire while a credential is being typed.
        if self.screen == Screen::Setup {
            return self.on_setup_key(key);
        }

        // The prompt is modal: while it is open, every character is content.
        if self.input_open {
            return self.on_prompt_key(key);
        }

        // So is the confirmation, and for the same reason: "y" must not have to
        // be spelled out in a help screen.
        if self.confirm.is_some() {
            return self.on_confirm_key(key);
        }

        match key.code {
            // `h` backs out for the same reason `Esc` does, and like Neovim it
            // is the key fingers reach for. `q` is deliberately unbound: quitting
            // is `Ctrl-C`, which cannot be typed by accident into a query.
            KeyCode::Esc | KeyCode::Char('h') => {
                self.back();
                None
            }
            KeyCode::Char('/') => {
                self.open_input();
                None
            }
            KeyCode::Char('?') => {
                self.toggle_help();
                None
            }
            KeyCode::Char('b') => {
                self.go(Screen::Budgets);
                Some(Intent::ReloadBudgets)
            }
            KeyCode::Char('s') => {
                self.go(Screen::Sessions);
                Some(Intent::ReloadSessions)
            }
            _ => self.on_screen_key(key),
        }
    }

    /// The first-run form. Unlike every other screen, `Esc` is not a way out:
    /// there is no home to return to until an account exists.
    fn on_setup_key(&mut self, key: KeyEvent) -> Option<Intent> {
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.setup.focus(false),
            KeyCode::BackTab | KeyCode::Up => self.setup.focus(true),
            KeyCode::Enter => {
                if let Some(missing) = self.setup.missing() {
                    self.notice_now(Level::Error, format!("enter {missing}"));
                    return None;
                }
                let credentials = self.setup.credentials();
                self.notice_now(Level::Info, "saving…");
                return Some(Intent::SaveCredentials(Box::new(credentials)));
            }
            KeyCode::Backspace => self.setup.input_mut().backspace(),
            KeyCode::Delete => self.setup.input_mut().delete(),
            KeyCode::Left => self.setup.input_mut().left(),
            KeyCode::Right => self.setup.input_mut().right(),
            KeyCode::Home => self.setup.input_mut().home(),
            KeyCode::End => self.setup.input_mut().end(),
            KeyCode::Char('\u{1}') => self.setup.input_mut().select_all(),
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.setup.input_mut().select_all();
            }
            KeyCode::Char(ch) => self.setup.input_mut().insert(ch),
            _ => {}
        }
        self.touch();
        None
    }

    fn on_prompt_key(&mut self, key: KeyEvent) -> Option<Intent> {
        match key.code {
            KeyCode::Esc => self.close_input(),
            KeyCode::Enter => {
                if self.input.is_blank() {
                    self.notice_now(Level::Error, "type a query, like `Artist - Album`");
                } else {
                    let text = self.input.text().trim().to_owned();
                    self.close_input();
                    self.screen = Screen::Results;
                    self.phase = Some(SearchPhase::Discogs);
                    self.error = None;
                    self.notice = None;
                    return Some(Intent::Search(text));
                }
            }
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End | KeyCode::Char('\u{5}') => self.input.end(),
            // Emacs and readline keys. A terminal may deliver Control-A as
            // `Char('a')` with the modifier set, or as the raw control byte with
            // no modifier at all, so both spellings are accepted rather than
            // betting on one.
            KeyCode::Char('\u{1}') => self.input.select_all(),
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.select_all();
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.end();
            }
            KeyCode::Char(ch) => self.input.insert(ch),
            _ => {}
        }
        None
    }

    fn on_confirm_key(&mut self, key: KeyEvent) -> Option<Intent> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') => {
                self.confirm = None;
                self.touch();
                None
            }
            KeyCode::Enter | KeyCode::Char('y') => {
                let confirm = self.confirm.take()?;
                self.download = Some(DownloadView::new(&confirm.prepared.plan));
                self.go(Screen::Download);
                Some(Intent::Download(Box::new(confirm.prepared)))
            }
            _ => None,
        }
    }

    fn on_screen_key(&mut self, key: KeyEvent) -> Option<Intent> {
        if self.files.is_some() {
            return self.on_files_key(key);
        }

        match self.screen {
            Screen::Results => self.on_results_key(key),
            Screen::Download => self.on_download_key(key),
            Screen::Budgets => match key.code {
                KeyCode::Char('r') => Some(Intent::ReloadBudgets),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.budget_row = self.budget_row.saturating_add(1);
                    None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.budget_row = self.budget_row.saturating_sub(1);
                    None
                }
                _ => None,
            },
            Screen::Sessions => match key.code {
                KeyCode::Char('r') => Some(Intent::ReloadSessions),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.session_row = self.session_row.saturating_add(1);
                    None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.session_row = self.session_row.saturating_sub(1);
                    None
                }
                _ => None,
            },
            Screen::Setup | Screen::Home | Screen::Help => None,
        }
    }

    fn on_results_key(&mut self, key: KeyEvent) -> Option<Intent> {
        match key.code {
            KeyCode::Tab => self.focus = self.focus.flipped(),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('g') => self.set_row(0),
            KeyCode::Char('G') => {
                let last = self.focused_len().saturating_sub(1);
                self.set_row(last);
            }
            KeyCode::Enter | KeyCode::Char('l') => {
                if self.focus == Focus::Discogs {
                    return self.choose_release();
                }
                self.open_files();
            }
            KeyCode::Char('t') => return self.choose_release(),
            KeyCode::Char('d') => return self.request_download(),
            _ => {}
        }
        self.touch();
        None
    }

    /// Picks the highlighted Discogs pressing for the selected source.
    fn choose_release(&mut self) -> Option<Intent> {
        if self.focus != Focus::Discogs {
            self.notice_now(Level::Info, "Tab to Discogs to choose a pressing");
            return None;
        }
        let Some(release_id) = self
            .discogs()
            .get(self.discogs_row)
            .map(|row| row.discogs_release_id.get())
        else {
            self.notice_now(Level::Error, "no Discogs match is selected");
            return None;
        };
        let Some(candidate_id) = self.selected_source().map(|source| source.candidate_id) else {
            self.notice_now(Level::Error, "no server folder is selected to tag");
            return None;
        };
        self.touch();
        Some(Intent::ChooseRelease {
            candidate_id,
            release_id,
        })
    }

    /// Opens the download confirmation for the focused source, or explains why
    /// it cannot.
    fn request_download(&mut self) -> Option<Intent> {
        if self.download_active() {
            self.notice_now(Level::Info, "a download is already running");
            return None;
        }
        if !self.config.server {
            self.notice_now(
                Level::Error,
                "SoulSeek is not configured: run `aldo login` in a terminal",
            );
            return None;
        }
        if self.focus != Focus::Sources {
            self.notice_now(Level::Info, "Tab to Sources to download a folder");
            return None;
        }
        let Some(candidate_id) = self.selected_source().map(|source| source.candidate_id) else {
            self.notice_now(Level::Error, "no server folder is selected");
            return None;
        };
        self.touch();
        Some(Intent::PrepareDownload(candidate_id))
    }

    /// Whether a download is still transferring.
    #[must_use]
    pub fn download_active(&self) -> bool {
        self.download.as_ref().is_some_and(DownloadView::is_active)
    }

    fn on_download_key(&mut self, key: KeyEvent) -> Option<Intent> {
        let view = self.download.as_mut()?;
        let last = view.files.len().saturating_sub(1);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => view.selected = (view.selected + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => view.selected = view.selected.saturating_sub(1),
            KeyCode::PageDown => view.selected = (view.selected + 10).min(last),
            KeyCode::PageUp => view.selected = view.selected.saturating_sub(10),
            KeyCode::Char('g') => view.selected = 0,
            KeyCode::Char('G') => view.selected = last,
            _ => {}
        }
        self.touch();
        None
    }

    fn on_files_key(&mut self, key: KeyEvent) -> Option<Intent> {
        let files = self.files.as_mut()?;
        let last = files.files.len().saturating_sub(1);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => files.selected = (files.selected + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => files.selected = files.selected.saturating_sub(1),
            KeyCode::PageDown => files.selected = (files.selected + 10).min(last),
            KeyCode::PageUp => files.selected = files.selected.saturating_sub(10),
            KeyCode::Char('g') => files.selected = 0,
            KeyCode::Char('G') => files.selected = last,
            _ => {}
        }
        self.touch();
        None
    }

    fn focused_len(&self) -> usize {
        match self.focus {
            Focus::Sources => self.sources().len(),
            Focus::Discogs => self.discogs().len(),
        }
    }

    fn set_row(&mut self, row: usize) {
        let clamped = row.min(self.focused_len().saturating_sub(1));
        match self.focus {
            Focus::Sources => self.source_row = clamped,
            Focus::Discogs => self.discogs_row = clamped,
        }
    }

    fn move_selection(&mut self, delta: i64) {
        let len = self.focused_len();
        if len == 0 {
            return;
        }
        let row = match self.focus {
            Focus::Sources => self.source_row,
            Focus::Discogs => self.discogs_row,
        };
        let moved = if delta >= 0 {
            (row + usize::try_from(delta).unwrap_or(0)).min(len - 1)
        } else {
            row.saturating_sub(usize::try_from(-delta).unwrap_or(0))
        };
        self.set_row(moved);
    }

    /// Opens the file list for the focused source, or explains why it cannot.
    fn open_files(&mut self) {
        if self.focus != Focus::Sources {
            self.notice_now(Level::Info, "Tab to Sources to open a folder");
            return;
        }
        let Some(candidate) = self.selected_source() else {
            self.notice_now(Level::Error, "no server folder is selected");
            return;
        };

        let peer = match &candidate.locator {
            aldo_core::candidate::Locator::SoulSeek(locator) => locator.peer.clone(),
            aldo_core::candidate::Locator::Torrent(_) => "-".to_owned(),
        };
        self.files = Some(FilesView {
            peer,
            folder: crate::format::folder(candidate),
            files: candidate.files.clone(),
            selected: 0,
        });
        self.touch();
    }
}

/// A one-line summary of a finished search, for the transient notice.
fn summary(run: &SearchRun) -> String {
    let folders = run.soulseek.sources().len().min(MAX_SOULSEEK_RESULTS);
    let matches = run.discogs.matches.len();
    match folders {
        0 => format!("{matches} Discogs matches, no server folders"),
        _ => format!("{folders} server folders, {matches} Discogs matches"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::crossterm::event::KeyEventKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn app() -> App {
        crate::tui::fixtures::app()
    }

    fn press(app: &mut App, code: KeyCode) -> Option<Intent> {
        app.update(Msg::Key(key(code)))
    }

    #[test]
    fn a_keystroke_is_a_key_event_of_the_expected_kind() {
        // Guards the fixture itself: a real terminal sends a Press event.
        assert_eq!(key(KeyCode::Enter).kind, KeyEventKind::Press);
    }

    #[test]
    fn only_ctrl_c_quits_and_q_is_never_it() {
        let mut app = app();
        // `q` is unbound: quitting is Ctrl-C, which cannot be typed by mistake.
        assert_eq!(press(&mut app, KeyCode::Char('q')), None, "q quit the app");
        assert_eq!(app.screen, Screen::Home, "q changed the screen");

        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.update(Msg::Key(ctrl_c)), Some(Intent::Quit));
    }

    #[test]
    fn q_inside_the_prompt_is_still_content() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('q'));
        assert_eq!(app.input.text(), "q");
    }

    #[test]
    fn h_backs_out_exactly_as_escape_does() {
        for key in [KeyCode::Esc, KeyCode::Char('h')] {
            let mut app = crate::tui::fixtures::with_results();
            press(&mut app, KeyCode::Char('b'));
            assert_eq!(app.screen, Screen::Budgets);

            assert_eq!(press(&mut app, key), None);
            assert_eq!(app.screen, Screen::Home, "{key:?} did not go back");
        }
    }

    #[test]
    fn the_prompt_is_modal_so_letters_never_trigger_commands() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        for ch in "budgets".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        assert_eq!(app.input.text(), "budgets");
        assert_eq!(app.screen, Screen::Home, "a letter changed the screen");

        press(&mut app, KeyCode::Esc);
        assert!(!app.input_open);
    }

    #[test]
    fn a_blank_query_is_refused_with_an_explanation() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert!(app.input_open, "an empty search closed the prompt");
        assert!(app.notice.is_some());
    }

    #[test]
    fn entering_a_query_asks_for_a_search_and_shows_progress() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        for ch in "a - b".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Intent::Search("a - b".to_owned()))
        );
        assert_eq!(app.screen, Screen::Results);
        assert_eq!(
            app.phase,
            Some(SearchPhase::Discogs),
            "no progress is shown"
        );
        assert!(!app.input_open);
    }

    #[test]
    fn escape_walks_back_through_the_screens() {
        let mut app = app();
        press(&mut app, KeyCode::Char('b'));
        assert_eq!(app.screen, Screen::Budgets);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.screen, Screen::Home);
    }

    #[test]
    fn help_remembers_where_it_was_opened_from() {
        let mut app = app();
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.screen, Screen::Sessions);
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.screen, Screen::Help);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.screen, Screen::Sessions);
    }

    #[test]
    fn budgets_and_sessions_ask_for_their_rows_when_opened() {
        let mut app = app();
        assert_eq!(
            press(&mut app, KeyCode::Char('b')),
            Some(Intent::ReloadBudgets)
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('s')),
            Some(Intent::ReloadSessions)
        );
    }

    #[test]
    fn the_prompt_edits_by_character_including_multibyte() {
        let mut input = Input::default();
        for ch in "café".chars() {
            input.insert(ch);
        }
        assert_eq!(input.text(), "café");
        input.backspace();
        assert_eq!(input.text(), "caf");
        input.home();
        input.insert('!');
        assert_eq!(input.text(), "!caf");
        input.delete();
        assert_eq!(input.text(), "!af");
    }

    fn ctrl(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    #[test]
    fn control_a_selects_everything_and_either_delete_key_clears_it() {
        for clear in [KeyCode::Backspace, KeyCode::Delete] {
            let mut app = app();
            press(&mut app, KeyCode::Char('/'));
            for ch in "bad brains".chars() {
                press(&mut app, KeyCode::Char(ch));
            }

            assert_eq!(app.update(Msg::Key(ctrl('a'))), None);
            assert!(app.input.all_selected(), "control-a did not select");

            assert_eq!(press(&mut app, clear), None);
            assert_eq!(app.input.text(), "", "{clear:?} left text behind");
            assert!(!app.input.all_selected());
        }
    }

    #[test]
    fn a_raw_soh_byte_selects_too_because_not_every_terminal_sends_a_modifier() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        for ch in "ok computer".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Char('\u{1}'));
        assert!(app.input.all_selected(), "the SOH spelling was ignored");
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        for ch in "old".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Char('a'));
        app.update(Msg::Key(ctrl('a')));

        press(&mut app, KeyCode::Char('n'));
        assert_eq!(
            app.input.text(),
            "n",
            "typing appended instead of replacing"
        );
        assert!(!app.input.all_selected(), "the selection outlived the edit");
    }

    #[test]
    fn control_e_jumps_to_the_end_and_drops_the_selection() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        for ch in "kid a".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Home);
        assert_eq!(app.input.cursor(), 0);

        app.update(Msg::Key(ctrl('a')));
        assert!(app.input.all_selected());

        app.update(Msg::Key(ctrl('e')));
        assert_eq!(app.input.cursor(), 5, "control-e did not reach the end");
        assert!(
            !app.input.all_selected(),
            "the selection survived control-e"
        );

        // And the caret still edits where it landed.
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(app.input.text(), "kid a!");
    }

    #[test]
    fn moving_the_caret_drops_the_selection() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        for ch in "kid a".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        app.update(Msg::Key(ctrl('a')));
        press(&mut app, KeyCode::Left);
        assert!(
            !app.input.all_selected(),
            "the selection survived a caret move"
        );

        // So backspace removes one character rather than the whole query. The
        // caret sat at the end, one left of it is the space.
        press(&mut app, KeyCode::Backspace);
        assert_eq!(
            app.input.text(),
            "kida",
            "backspace did not remove one character"
        );
    }

    #[test]
    fn moving_the_selection_never_leaves_the_list() {
        let mut app = app();
        app.focus = Focus::Sources;
        app.move_selection(1);
        assert_eq!(app.source_row, 0, "an empty list moved the selection");
    }

    #[test]
    fn the_first_run_opens_the_credential_form() {
        let app = App::new(crate::tui::fixtures::needs_setup(), None);
        assert_eq!(app.screen, Screen::Setup);
        assert!(app.setup.missing().is_some(), "nothing is required");
    }

    #[test]
    fn the_form_will_not_submit_without_both_soulseek_fields() {
        let mut app = App::new(crate::tui::fixtures::needs_setup(), None);

        // A password with no username.
        press(&mut app, KeyCode::Tab);
        for ch in "secret".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert_eq!(app.screen, Screen::Setup, "an incomplete form submitted");
        assert!(app.notice.is_some(), "the refusal was silent");
    }

    #[test]
    fn completing_the_form_asks_to_save_the_credentials() {
        let mut app = App::new(crate::tui::fixtures::needs_setup(), None);
        for ch in "alice".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Tab);
        for ch in "secret".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Tab);
        for ch in "tok".chars() {
            press(&mut app, KeyCode::Char(ch));
        }

        match press(&mut app, KeyCode::Enter).expect("a save intent") {
            Intent::SaveCredentials(credentials) => {
                assert_eq!(credentials.slsk_username.as_deref(), Some("alice"));
                assert_eq!(credentials.slsk_password.as_deref(), Some("secret"));
                assert_eq!(credentials.discogs_token.as_deref(), Some("tok"));
            }
            other => panic!("expected a save, got {other:?}"),
        }
    }

    #[test]
    fn a_paste_lands_in_the_focused_setup_field() {
        let mut app = App::new(crate::tui::fixtures::needs_setup(), None);
        app.setup.field = SetupField::Token;
        app.update(Msg::Paste("abc123\nextra".to_owned()));

        assert_eq!(app.setup.token.text(), "abc123extra", "a newline was kept");
        assert_eq!(app.screen, Screen::Setup, "a paste submitted the form");
        assert!(
            app.setup.token.is_secret(),
            "the token stopped being hidden"
        );
    }

    #[test]
    fn a_saved_form_leaves_the_setup_screen() {
        let mut app = App::new(crate::tui::fixtures::needs_setup(), None);
        let intent = app.update(Msg::SetupSaved(Box::new(Ok(credentials()))));
        assert_eq!(intent, None);
        assert_eq!(app.screen, Screen::Home);
    }

    #[test]
    fn a_query_from_the_command_line_waits_for_the_form() {
        let mut app = App::new(
            crate::tui::fixtures::needs_setup(),
            Some("a - b".to_owned()),
        );
        assert_eq!(app.screen, Screen::Setup, "the query skipped setup");
        assert!(app.phase.is_none(), "progress showed before signing in");

        let intent = app.update(Msg::SetupSaved(Box::new(Ok(credentials()))));
        assert_eq!(intent, Some(Intent::Search("a - b".to_owned())));
        assert_eq!(app.screen, Screen::Results);
        assert_eq!(app.phase, Some(SearchPhase::Discogs));
    }

    #[test]
    fn a_failed_save_keeps_the_form_open_with_the_reason() {
        let mut app = App::new(crate::tui::fixtures::needs_setup(), None);
        app.update(Msg::SetupSaved(Box::new(Err("disk full".to_owned()))));
        assert_eq!(app.screen, Screen::Setup);
        assert_eq!(
            app.notice.as_ref().map(|notice| notice.text.as_str()),
            Some("disk full")
        );
    }

    fn credentials() -> Credentials {
        Credentials {
            slsk_username: Some("alice".to_owned()),
            slsk_password: Some("secret".to_owned()),
            discogs_token: None,
        }
    }

    #[test]
    fn a_query_from_the_command_line_runs_immediately_and_only_once() {
        let mut app = App::new(
            ConfigSummary {
                library_root: PathBuf::from("/music"),
                discogs: true,
                server: true,
            },
            Some("radiohead - ok computer".to_owned()),
        );

        assert_eq!(app.screen, Screen::Results, "it did not open the results");
        assert_eq!(app.phase, Some(SearchPhase::Discogs));
        assert_eq!(
            app.take_pending_search().as_deref(),
            Some("radiohead - ok computer")
        );
        assert!(app.take_pending_search().is_none(), "the query ran twice");
    }

    #[test]
    fn an_empty_command_line_query_just_opens_the_box() {
        let mut app = App::new(
            ConfigSummary {
                library_root: PathBuf::from("/music"),
                discogs: true,
                server: true,
            },
            Some("   ".to_owned()),
        );
        assert_eq!(app.screen, Screen::Home);
        assert!(app.take_pending_search().is_none());
    }

    #[test]
    fn a_pasted_newline_is_not_an_enter_key() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.update(Msg::Paste("a - b\nc".to_owned())), None);
        assert_eq!(app.input.text(), "a - bc");
        assert_eq!(app.screen, Screen::Home, "a paste started a search");
    }

    #[test]
    fn d_asks_to_prepare_a_download_for_the_focused_folder() {
        let mut app = crate::tui::fixtures::with_results();

        assert_eq!(
            press(&mut app, KeyCode::Char('d')),
            Some(Intent::PrepareDownload(
                crate::tui::fixtures::source().candidate_id
            ))
        );
    }

    #[test]
    fn d_on_the_discogs_pane_explains_itself_instead_of_downloading() {
        let mut app = crate::tui::fixtures::with_results();
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Discogs);
        assert_eq!(press(&mut app, KeyCode::Char('d')), None);
        assert!(app.notice.is_some(), "nothing explained the refusal");
    }

    #[test]
    fn enter_on_a_pressing_asks_to_store_it_for_the_selected_folder() {
        let mut app = crate::tui::fixtures::with_results();
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Discogs);

        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Intent::ChooseRelease {
                candidate_id: crate::tui::fixtures::source().candidate_id,
                release_id: 26_707_685,
            })
        );
    }

    #[test]
    fn t_on_the_folder_list_explains_that_it_is_for_the_discogs_pane() {
        let mut app = crate::tui::fixtures::with_results();
        assert_eq!(press(&mut app, KeyCode::Char('t')), None);
        assert!(app.notice.is_some(), "nothing explained the refusal");
    }

    #[test]
    fn confirming_opens_the_download_screen_and_runs_it() {
        let mut app = crate::tui::fixtures::with_results();
        app.confirm = Some(Confirm {
            prepared: crate::tui::fixtures::prepared(),
            library_root: PathBuf::from("/home/you/Music"),
        });

        let intent = press(&mut app, KeyCode::Enter).expect("a download intent");
        assert!(matches!(intent, Intent::Download(_)));
        assert_eq!(app.screen, Screen::Download);
        assert!(app.confirm.is_none(), "the modal stayed open");
        assert!(app.download_active(), "the view is not shown as active");
    }

    #[test]
    fn cancelling_leaves_the_results_untouched() {
        let mut app = crate::tui::fixtures::with_results();
        app.confirm = Some(Confirm {
            prepared: crate::tui::fixtures::prepared(),
            library_root: PathBuf::from("/home/you/Music"),
        });

        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert!(app.confirm.is_none());
        assert_eq!(app.screen, Screen::Results);
        assert!(app.download.is_none(), "a download started anyway");
    }

    #[test]
    fn a_second_download_is_refused_while_one_is_running() {
        let mut app = crate::tui::fixtures::with_results();
        app.download = Some(DownloadView::new(&crate::tui::fixtures::plan()));
        assert!(app.download_active());

        assert_eq!(press(&mut app, KeyCode::Char('d')), None);
        assert!(app.notice.is_some(), "the refusal was silent");
    }

    #[test]
    fn the_download_view_counts_bytes_and_tracks_each_file() {
        let mut view = DownloadView::new(&crate::tui::fixtures::plan());
        let total = view.total_bytes;
        assert_eq!(view.done_bytes(), 0);
        assert!(view.is_active());

        view.apply(&DownloadEvent::FileStarted {
            index: 1,
            total: 2,
            name: "a.flac".to_owned(),
            size: aldo_core::audio::ByteSize::new(30_000_000),
        });
        view.apply(&DownloadEvent::Progress {
            bytes_done: 15_000_000,
            bytes_total: 30_000_000,
            speed_bps: 1_024,
            queued: false,
        });
        assert_eq!(
            view.done_bytes(),
            15_000_000,
            "the active file is not counted"
        );

        view.apply(&DownloadEvent::FileDone { index: 1 });
        assert_eq!(view.files[0].state, FileState::Done);
        assert_eq!(view.done_bytes(), 30_000_000);
        assert!(view.current.is_none());

        view.apply(&DownloadEvent::FileSkipped {
            index: 2,
            total: 2,
            name: "b.flac".to_owned(),
        });
        assert_eq!(view.files[1].state, FileState::Skipped);
        assert_eq!(view.done_bytes(), total, "the total was not reached");
    }

    #[test]
    fn a_finished_download_is_terminal_so_another_can_start() {
        let mut app = crate::tui::fixtures::with_results();
        let plan = crate::tui::fixtures::plan();
        let mut view = DownloadView::new(&plan);
        app.download = Some(view.clone());
        assert!(app.download_active());

        app.update(Msg::DownloadResult(Ok(())));
        assert!(!app.download_active(), "a finished download still blocks");
        assert_eq!(
            app.download.as_ref().map(|view| view.stage),
            Some(DownloadStage::Done)
        );
        let _ = &mut view;
    }

    #[test]
    fn a_failed_download_reports_the_reason_on_the_view() {
        let mut app = crate::tui::fixtures::with_results();
        app.download = Some(DownloadView::new(&crate::tui::fixtures::plan()));

        app.update(Msg::DownloadResult(Err("peer vanished".to_owned())));

        let view = app.download.as_ref().expect("a view");
        assert_eq!(view.stage, DownloadStage::Failed);
        assert_eq!(view.message, "peer vanished");
        assert!(!view.is_active());
    }
}
