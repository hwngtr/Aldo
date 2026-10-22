//! Drawing. The only module that touches a terminal.
//!
//! The layout is two fixed rows — a header and a footer with the keys — around
//! a body that changes with the screen. Modals draw over the body last, so the
//! background stays visible behind them.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use aldo_core::audio::ByteSize;

use crate::actions::reports::{BudgetLine, age};
use crate::actions::search::{SearchPhase, SoulSeekReport};
use crate::format::{basename, folder, quality, short_duration, speed};
use crate::tui::app::{
    App, Confirm, DownloadStage, DownloadView, FileState, FilesView, Focus, Input, Level, Screen,
    SetupField,
};
use crate::tui::theme;

/// The animation, when a provider is being asked.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(if app.notice.is_some() { 2 } else { 1 }),
    ])
    .split(area);

    header(frame, app, rows[0]);
    body(frame, app, rows[1]);
    footer(frame, app, rows[2]);

    if let Some(files) = &app.files {
        files_view(frame, files, rows[1]);
    }
    if let Some(confirm) = &app.confirm {
        confirm_overlay(frame, confirm, area);
    }
    if app.screen == Screen::Help {
        help_overlay(frame, area);
    }

    app.mark_drawn();
}

/// The wordmark on the left, the current state on the right.
fn header(frame: &mut Frame, app: &App, area: Rect) {
    let left = if app.screen == Screen::Home {
        // The home screen draws the wordmark itself; repeating it here would
        // read as a bug.
        Line::default()
    } else {
        Line::from(vec![
            Span::styled(" ALDO ", theme::wordmark()),
            Span::styled(" lossless album finder", theme::muted()),
        ])
    };

    let (marker, text, style) = match &app.error {
        Some(error) => ("✗", head(first_line(error), 60), theme::bad()),
        None => match (&app.phase, app.download.as_ref()) {
            // A transfer outranks a search: it is the thing still moving.
            (_, Some(view)) if view.is_active() => {
                let tick = app.spinner % u64::try_from(SPINNER.len()).unwrap_or(1);
                let spinner = SPINNER[usize::try_from(tick).unwrap_or(0)];
                (
                    spinner,
                    format!("downloading {}", view.message),
                    theme::accent(),
                )
            }
            (Some(phase), _) => {
                let tick = app.spinner % u64::try_from(SPINNER.len()).unwrap_or(1);
                let spinner = SPINNER[usize::try_from(tick).unwrap_or(0)];
                (spinner, phase_label(*phase).to_owned(), theme::accent())
            }
            _ => ("●", "ready".to_owned(), theme::good()),
        },
    };
    let right = Line::from(vec![
        Span::styled(format!("{marker} "), style),
        Span::styled(text, style),
        Span::raw(" "),
    ])
    .right_aligned();

    // Two columns rather than two paragraphs over one area: a second paragraph
    // would blank the trailing cells of the first.
    let columns = Layout::horizontal([Constraint::Min(10), Constraint::Length(34)]).split(area);
    frame.render_widget(Paragraph::new(left), columns[0]);
    frame.render_widget(Paragraph::new(right), columns[1]);
}

fn body(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.screen {
        Screen::Setup => setup(frame, app, area),
        Screen::Home => home(frame, app, area),
        Screen::Results => results(frame, app, area),
        Screen::Download => download(frame, app, area),
        Screen::Budgets => budgets(frame, app, area),
        Screen::Sessions => sessions(frame, app, area),
        Screen::Help => {}
    }
}

/// A small terminal gets the plain wordmark rather than a clipped logo. The art
/// needs its own rows plus the tagline, the search box and the footer, with a
/// little slack; below that the spaced capitals keep the screen usable.
const WORDMARK_MIN_HEIGHT: u16 = 12;

fn home(frame: &mut Frame, app: &App, area: Rect) {
    let room = area.width >= u16::try_from(super::wordmark::WIDTH + 5).unwrap_or(u16::MAX)
        && area.height >= WORDMARK_MIN_HEIGHT;
    let hero = if room {
        u16::try_from(super::wordmark::ROWS).unwrap_or(1)
    } else {
        1
    };

    // Equal slack above and below centres the block. The blank row before the
    // search box keeps the wordmark from crowding it.
    let rows = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(hero),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .split(area);

    if room {
        let lines: Vec<Line> = super::wordmark::rows()
            .into_iter()
            .map(|row| Line::from(Span::styled(row, theme::accent())).centered())
            .collect();
        frame.render_widget(Paragraph::new(lines), rows[1]);
    } else {
        frame.render_widget(
            Paragraph::new(Line::from(super::wordmark::PLAIN).centered()).style(theme::wordmark()),
            rows[1],
        );
    }
    frame.render_widget(
        Paragraph::new(Line::from("find the release, then find the files").centered())
            .style(theme::muted()),
        rows[2],
    );

    // The search box is the one thing the eye should land on.
    let width = area.width.saturating_sub(8).clamp(20, 66);
    let box_area = centered(rows[4], width, 3);
    let block = Block::bordered()
        .border_style(if app.input_open {
            theme::accent()
        } else {
            theme::muted()
        })
        .title(Span::styled(" search ", theme::label()));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);
    frame.render_widget(Paragraph::new(input_line(app)), inner);

    // The hint says what the next keystroke will do, which is the one thing a
    // hidden mode needs to say out loud.
    frame.render_widget(
        Paragraph::new(Line::from(input_hint(app)).centered()).style(theme::muted()),
        rows[5],
    );
    frame.render_widget(Paragraph::new(config_line(app)).centered(), rows[6]);
}

/// The prompt and its contents, with a block caret where the next character
/// would land.
fn input_hint(app: &App) -> &'static str {
    if app.input.all_selected() {
        "Backspace or Delete clears the query"
    } else {
        "type `Artist - Album`, then press Enter"
    }
}

fn input_line(app: &App) -> Line<'static> {
    if !app.input_open && app.input.text().is_empty() {
        return Line::from(vec![
            Span::styled("> ", theme::accent()),
            Span::styled("press / to search", theme::muted()),
        ]);
    }

    let mut spans = vec![Span::styled("> ", theme::accent())];
    spans.extend(editable_line(&app.input, app.input_open, false).spans);
    Line::from(spans)
}

/// One field's contents, with a block caret where it is focused and the
/// characters drawn as marks when the field is a secret.
fn editable_line(input: &Input, focused: bool, secret: bool) -> Line<'static> {
    let text = if secret {
        "•".repeat(input.text().chars().count())
    } else {
        input.text().to_owned()
    };

    // A selection is drawn as a block of highlight with no caret inside it.
    if input.all_selected() {
        return Line::from(Span::styled(text, theme::selected()));
    }

    let (before, after) = split_at_cursor(&text, input, focused);
    let mut spans = vec![Span::styled(before, theme::strong())];
    if focused {
        spans.push(Span::styled(" ", Style::new().bg(theme::ACCENT)));
    }
    spans.push(Span::styled(after, theme::strong()));
    Line::from(spans)
}

/// Splits the text at the caret, by character, so a multibyte query is safe.
fn split_at_cursor(text: &str, input: &Input, caret: bool) -> (String, String) {
    if !caret {
        return (text.to_owned(), String::new());
    }
    // `Input` keeps the caret private; the visible split is derived from the
    // same character counting it uses.
    let chars: Vec<char> = text.chars().collect();
    let cursor = input.cursor().min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let after: String = chars[cursor..].iter().collect();
    (before, after)
}

fn config_line(app: &App) -> Line<'static> {
    let chip = |ok: bool, label: &str| {
        if ok {
            Span::styled(format!("✓ {label}"), theme::good())
        } else {
            Span::styled(format!("✗ {label}"), theme::bad())
        }
    };

    Line::from(vec![
        chip(app.config.discogs, "Discogs"),
        Span::styled("   ", theme::muted()),
        chip(app.config.server, "SoulSeek"),
        Span::styled("   ", theme::muted()),
        Span::styled(
            format!("library {}", app.config.library_root.display()),
            theme::muted(),
        ),
    ])
}

/// The first-run credential form. It replaces the body rather than floating,
/// because it is the only thing there is to do until it is submitted.
fn setup(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width.saturating_sub(8).clamp(40, 72);
    let box_area = centered(area, width, 13);
    let block = Block::bordered().title(section_title("sign in", "", theme::label()));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let rows = Layout::vertical([
        Constraint::Length(1), // heading
        Constraint::Length(1), // spacer
        Constraint::Length(1), // username
        Constraint::Length(1), // password
        Constraint::Length(1), // token
        Constraint::Length(1), // spacer
        Constraint::Min(1),    // notes
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Aldo needs a SoulSeek account. Saved once, used from now on.",
            theme::strong(),
        ))),
        rows[0],
    );

    let field = app.setup.field;
    field_row(
        frame,
        rows[2],
        "SoulSeek user",
        &app.setup.username,
        field == SetupField::Username,
        "your account name",
    );
    field_row(
        frame,
        rows[3],
        "SoulSeek pass",
        &app.setup.password,
        field == SetupField::Password,
        "your password",
    );
    field_row(
        frame,
        rows[4],
        "Discogs token",
        &app.setup.token,
        field == SetupField::Token,
        "optional, from discogs.com/settings/developers",
    );

    let notes = vec![
        Line::from(Span::styled(
            " Tab moves between fields, Enter saves.",
            theme::muted(),
        )),
        Line::from(Span::styled(
            " The Discogs token may be blank; downloads are then untagged.",
            theme::muted(),
        )),
        Line::from(Span::styled(
            " Sign out later with `aldo logout`.",
            theme::muted(),
        )),
    ];
    frame.render_widget(Paragraph::new(notes).wrap(Wrap { trim: false }), rows[6]);
}

/// One labelled field, empty ones showing a placeholder instead of nothing.
fn field_row(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    input: &Input,
    focused: bool,
    placeholder: &str,
) {
    let style = if focused {
        theme::accent()
    } else {
        theme::muted()
    };
    let marker = if focused { "▸" } else { " " };

    let mut spans = vec![Span::styled(format!(" {marker} {label:<14} "), style)];
    if input.text().is_empty() && !focused {
        spans.push(Span::styled(placeholder.to_owned(), theme::muted()));
    } else {
        spans.extend(editable_line(input, focused, input.is_secret()).spans);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn results(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.search.is_none() {
        let message = if let Some(error) = &app.error {
            Line::from(vec![
                Span::styled("search failed: ", theme::bad()),
                Span::raw(error.clone()),
            ])
        } else {
            Line::from(Span::styled("asking the network…", theme::muted()))
        };
        frame.render_widget(Paragraph::new(message).centered(), centered(area, 60, 1));
        return;
    }

    // Each panel is as tall as its content, and the remainder is left blank
    // rather than stretching an empty box down the screen. A search that reaches
    // no peers is a common state, and a tall empty bordered panel reads as a
    // rendering fault.
    let discogs = discogs_height(app);
    let sources = sources_height(app).min(area.height.saturating_sub(discogs).max(3));
    let rows = Layout::vertical([
        Constraint::Length(sources),
        Constraint::Length(discogs),
        Constraint::Fill(1),
    ])
    .split(area);
    sources_panel(frame, app, rows[0]);
    discogs_panel(frame, app, rows[1]);
}

/// Two borders, a header, and one row per folder.
fn sources_height(app: &App) -> u16 {
    let folders = u16::try_from(app.sources().len()).unwrap_or(u16::MAX);
    if folders == 0 {
        // Room for the explanation that stands in for the empty table.
        return 6;
    }
    folders.saturating_add(4).min(15)
}

fn discogs_height(app: &App) -> u16 {
    // Two borders, a header, and one row per match, capped so a long match list
    // cannot push the folder list off a short terminal.
    let matches = u16::try_from(app.discogs().len()).unwrap_or(u16::MAX);
    matches.saturating_add(3).min(10)
}

fn sources_panel(frame: &mut Frame, app: &App, area: Rect) {
    let title = match app.search.as_ref().map(|run| &run.soulseek) {
        Some(SoulSeekReport::Answered(found)) => {
            let shown = app.sources().len();
            let extra = if found.candidates.len() > shown {
                format!(" · showing {shown} of {}", found.candidates.len())
            } else {
                String::new()
            };
            section_title(
                "SOULSEEK",
                &format!("{}{extra}", plural(shown, "folder")),
                theme::label(),
            )
        }
        Some(SoulSeekReport::RateLimited { retry_after }) => section_title(
            "SOULSEEK",
            &format!("rate limited, retry in {}", short_duration(*retry_after)),
            theme::warn(),
        ),
        Some(SoulSeekReport::Unreachable { message }) => {
            section_title("SOULSEEK", first_line(message), theme::bad())
        }
        Some(SoulSeekReport::NotConfigured) => {
            section_title("SOULSEEK", "not configured: run `aldo login`", theme::bad())
        }
        Some(SoulSeekReport::Refused) => section_title(
            "SOULSEEK",
            "sign-in rejected: check the account, then run `aldo login`",
            theme::bad(),
        ),
        None => section_title("SOULSEEK", "not asked", theme::muted()),
    };

    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("folder"),
        Cell::from("peer"),
        Cell::from("files"),
        Cell::from("size"),
        Cell::from("quality"),
        Cell::from("speed"),
        Cell::from("slot"),
    ])
    .style(theme::label());

    let widths = [
        Constraint::Length(3),
        Constraint::Min(18),
        Constraint::Length(14),
        Constraint::Length(6),
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(12),
        Constraint::Length(9),
    ];

    let rows: Vec<Row> = app
        .sources()
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let locator = match &candidate.locator {
                aldo_core::candidate::Locator::SoulSeek(locator) => locator,
                aldo_core::candidate::Locator::Torrent(_) => {
                    return Row::new(vec![Cell::from(format!("{}", index + 1))]);
                }
            };
            let slot = if locator.has_free_slot {
                Cell::from("free").style(theme::good())
            } else {
                Cell::from("queued").style(theme::muted())
            };
            Row::new(vec![
                Cell::from(format!("{}", index + 1)),
                Cell::from(folder(candidate)),
                Cell::from(crate::out::terminal_safe(&locator.peer)),
                Cell::from(candidate.display.track_count.to_string()),
                Cell::from(candidate.total_bytes.to_string()),
                Cell::from(quality(locator.bit_depth, locator.sample_rate)),
                Cell::from(speed(locator.upload_speed_bps)),
                slot,
            ])
        })
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(theme::selected())
        .highlight_symbol("▸ ")
        .block(Block::bordered().title(title));

    let selected = if app.focus == Focus::Sources {
        Some(app.source_row)
    } else {
        None
    };
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);

    if app.sources().is_empty() {
        empty_folder_hint(frame, app, area);
    }
}

/// An empty list usually means something fixable, so the reason is drawn over
/// the empty body, where the eye already is.
fn empty_folder_hint(frame: &mut Frame, app: &App, area: Rect) {
    let Some(reason) = app.soulseek_hint() else {
        return;
    };

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("  {reason}"),
            theme::warn(),
        )))
        .wrap(Wrap { trim: true }),
        Block::bordered().inner(area),
    );
}

fn discogs_panel(frame: &mut Frame, app: &App, area: Rect) {
    let title = match app.search.as_ref() {
        Some(run) => section_title(
            "DISCOGS",
            &format!("{}  ·  {}", run.discogs.detail, run.discogs.matches.len()),
            theme::label(),
        ),
        None => section_title("DISCOGS", "", theme::muted()),
    };

    let header = Row::new(vec![
        Cell::from("id"),
        Cell::from("release"),
        Cell::from("artist"),
        Cell::from("year"),
        Cell::from("type"),
        Cell::from("tracks"),
    ])
    .style(theme::label());

    let widths = [
        Constraint::Length(10),
        Constraint::Min(20),
        Constraint::Length(20),
        Constraint::Length(6),
        Constraint::Length(10),
        Constraint::Length(7),
    ];

    let rows: Vec<Row> = app
        .discogs()
        .iter()
        .map(|row| {
            Row::new(vec![
                Cell::from(row.discogs_release_id.to_string()),
                Cell::from(row.title.clone()),
                Cell::from(row.artist.clone().unwrap_or_else(|| "unknown".to_owned())),
                Cell::from(
                    row.year
                        .map_or_else(|| "----".to_owned(), |year| year.to_string()),
                ),
                Cell::from(
                    row.release_types
                        .iter()
                        .map(|kind| kind.as_vorbis_value())
                        .collect::<Vec<_>>()
                        .join("/"),
                ),
                Cell::from(row.track_count.to_string()),
            ])
        })
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(theme::selected())
        .highlight_symbol("▸ ")
        .block(Block::bordered().title(title));

    let selected = if app.focus == Focus::Discogs {
        Some(app.discogs_row)
    } else {
        None
    };
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
}

fn download(frame: &mut Frame, app: &App, area: Rect) {
    let Some(view) = app.download.as_ref() else {
        frame.render_widget(
            Paragraph::new(Span::styled("no download yet", theme::muted())).centered(),
            centered(area, 40, 1),
        );
        return;
    };

    let (marker, style) = match view.stage {
        // `→` rather than the prettier `⏵`: no font in this environment draws
        // U+23F5, and `→` is in every monospace face, so the marker is tofu in
        // no terminal.
        DownloadStage::Connecting | DownloadStage::Running => ("→", theme::accent()),
        DownloadStage::Done => ("✓", theme::good()),
        DownloadStage::Failed => ("✗", theme::bad()),
    };

    let title = Line::from(vec![
        Span::styled(" DOWNLOAD ", theme::label()),
        Span::styled(format!("{} ", view.title), theme::strong()),
        Span::styled(format!(" {marker} {} ", view.message), style),
    ]);

    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(3),
    ])
    .split(inner);

    // Overall progress, measured in bytes so a big first file does not stall the
    // bar visibly.
    let done = view.done_bytes();
    let fraction = if view.total_bytes == 0 {
        1
    } else {
        fill(
            u32::try_from(done.min(u64::from(u32::MAX))).unwrap_or(u32::MAX),
            u32::try_from(view.total_bytes.min(u64::from(u32::MAX))).unwrap_or(u32::MAX),
            usize::from(rows[0].width),
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("█".repeat(fraction), theme::accent()),
            Span::styled(
                "░".repeat(usize::from(rows[0].width).saturating_sub(fraction)),
                theme::muted(),
            ),
        ])),
        rows[0],
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{} ", ByteSize::new(done)), theme::strong()),
            Span::styled(
                format!("of {}   ·   ", ByteSize::new(view.total_bytes)),
                theme::muted(),
            ),
            Span::styled(
                format!(
                    "{} files   ·   peer {}",
                    view.files.len(),
                    crate::out::terminal_safe(&view.peer)
                ),
                theme::muted(),
            ),
        ])),
        rows[1],
    );

    current_file(frame, view, rows[2]);

    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("file"),
        Cell::from("size"),
        Cell::from("state"),
    ])
    .style(theme::label());

    let rows_file: Vec<Row> = view
        .files
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let (label, style) = state_label(line.state);
            Row::new(vec![
                Cell::from((index + 1).to_string()),
                Cell::from(crate::out::terminal_safe(&line.name)),
                Cell::from(ByteSize::new(line.size).to_string()),
                Cell::from(label).style(style),
            ])
        })
        .collect();

    let table = Table::new(
        rows_file,
        [
            Constraint::Length(4),
            Constraint::Min(24),
            Constraint::Length(10),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .column_spacing(1)
    .row_highlight_style(theme::selected())
    .highlight_symbol("▸ ")
    .block(Block::bordered());
    let mut state = TableState::default().with_selected(Some(view.selected));
    frame.render_stateful_widget(table, rows[4], &mut state);
}

/// The file being transferred, with its own bar and speed.
fn current_file(frame: &mut Frame, view: &DownloadView, area: Rect) {
    let Some(current) = view.current.as_ref() else {
        let text = match view.stage {
            DownloadStage::Done => {
                Line::from(Span::styled("all files are in place", theme::good()))
            }
            DownloadStage::Failed => Line::from(Span::styled(&view.message, theme::bad())),
            _ => Line::from(Span::styled("waiting for the peer…", theme::muted())),
        };
        frame.render_widget(Paragraph::new(text), area);
        return;
    };

    let filled = fill(
        u32::try_from(current.bytes_done.min(u64::from(u32::MAX))).unwrap_or(u32::MAX),
        u32::try_from(current.bytes_total.min(u64::from(u32::MAX))).unwrap_or(u32::MAX),
        usize::from(area.width).saturating_sub(40).clamp(4, 24),
    );
    let width = usize::from(area.width).saturating_sub(40).clamp(4, 24);
    let status = if current.queued {
        "waiting in queue".to_owned()
    } else {
        speed(current.speed_bps)
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(
                format!("[{}/{}] ", current.index, view.files.len()),
                theme::label(),
            ),
            Span::styled(head(&current.name, 60), theme::strong()),
        ]),
        Line::from(vec![
            Span::styled("█".repeat(filled), theme::accent()),
            Span::styled("░".repeat(width.saturating_sub(filled)), theme::muted()),
            Span::styled(
                format!(
                    "  {} / {}  ·  {status}",
                    ByteSize::new(current.bytes_done),
                    ByteSize::new(current.bytes_total)
                ),
                theme::muted(),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

fn state_label(state: FileState) -> (&'static str, Style) {
    match state {
        FileState::Pending => ("waiting", theme::muted()),
        FileState::Active => ("downloading", theme::accent()),
        FileState::Done => ("done", theme::good()),
        FileState::Skipped => ("already here", theme::good()),
    }
}

/// The confirmation before anything is written.
fn confirm_overlay(frame: &mut Frame, confirm: &Confirm, area: Rect) {
    let overlay = centered(area, area.width.saturating_sub(8).min(76), 9);
    frame.render_widget(Clear, overlay);

    let plan = &confirm.prepared.plan;
    let target = crate::actions::download::target_directory(
        &confirm.library_root,
        &plan.artist,
        &plan.album,
    );
    let mut lines = vec![
        Line::from(Span::styled(format!(" {}", plan.title()), theme::strong())),
        Line::default(),
        Line::from(vec![
            Span::styled("  from ", theme::muted()),
            Span::styled(plan.peer().unwrap_or("unknown").to_owned(), theme::label()),
            Span::styled(
                format!(
                    "   {}   {}",
                    plural(plan.files.len(), "file"),
                    plan.total_bytes
                ),
                theme::muted(),
            ),
        ]),
        Line::from(vec![
            Span::styled("  into ", theme::muted()),
            Span::styled(target.display().to_string(), theme::strong()),
        ]),
        Line::default(),
    ];
    lines.push(match confirm.prepared.tags.as_ref() {
        Some(tags) => Line::from(Span::styled(
            format!(
                "  Discogs tags will be written  ·  release {}  ·  {} tracks",
                tags.discogs_release_id,
                tags.tracks.len()
            ),
            theme::good(),
        )),
        None => Line::from(Span::styled(
            "  no tags: Discogs metadata is missing or does not match",
            theme::warn(),
        )),
    });
    if let Some(warning) = &confirm.prepared.warning {
        lines.push(Line::from(Span::styled(
            format!("  {warning}"),
            theme::warn(),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(section_title(
                "download?",
                "Enter to start   ·   Esc to cancel",
                theme::label(),
            ))),
        overlay,
    );
}

fn budgets(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(section_title(
        "RATE BUDGETS",
        "exceeding SoulSeek's is a 30-minute ban",
        theme::label(),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.budgets.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("reading…", theme::muted())),
            inner,
        );
        return;
    }

    let width = usize::from(inner.width.saturating_sub(48)).clamp(8, 40);
    let lines: Vec<Line> = app
        .budgets
        .iter()
        .enumerate()
        .map(|(index, line)| budget_line(line, width, index == app.budget_row))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn budget_line(line: &BudgetLine, width: usize, selected: bool) -> Line<'static> {
    let filled = fill(line.available, line.capacity, width);
    let bar_style = if line.is_limited() {
        theme::warn()
    } else if line.available == 0 {
        theme::bad()
    } else {
        theme::good()
    };

    let wait = if line.retry_after.is_zero() {
        String::new()
    } else {
        format!("  wait {}", short_duration(line.retry_after))
    };

    let style = if selected {
        theme::selected()
    } else {
        Style::new()
    };
    Line::from(vec![
        Span::styled(
            format!("{:<12}", line.provider),
            style.patch(theme::label()),
        ),
        Span::styled("█".repeat(filled), style.patch(bar_style)),
        Span::styled("░".repeat(width - filled), style.patch(theme::muted())),
        Span::styled(
            format!(
                "  {:>3}/{:<3}  {} per {}{wait}",
                line.available,
                line.capacity,
                line.capacity,
                short_duration(line.window),
            ),
            style,
        ),
    ])
}

fn sessions(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(section_title("RECENT SEARCHES", "", theme::label()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.sessions.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("no searches yet", theme::muted())),
            inner,
        );
        return;
    }

    let now = aldo_store::now_ms();
    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("when"),
        Cell::from("found"),
        Cell::from("query"),
    ])
    .style(theme::label());

    let rows: Vec<Row> = app
        .sessions
        .iter()
        .map(|row| {
            Row::new(vec![
                Cell::from(row.session_id.to_string()),
                Cell::from(age(row.created_at_ms, now)),
                Cell::from(row.result_count.to_string()),
                Cell::from(row.phrase.clone()),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .column_spacing(1)
    .row_highlight_style(theme::selected())
    .highlight_symbol("▸ ")
    .block(Block::bordered());
    let mut state = TableState::default().with_selected(Some(app.session_row));
    frame.render_stateful_widget(table, inner, &mut state);
}

/// The files in one folder.
///
/// A panel the size of the body rather than a floating modal: a modal leaves the
/// folder list's row numbers visible beside the file numbers, and two numbered
/// columns in one view read as one list. It is also what the other list screens
/// do, so there is one way to look at a list of things.
fn files_view(frame: &mut Frame, files: &FilesView, area: Rect) {
    // The results are still drawn underneath, and a table only writes the cells
    // its own text and rules occupy, so without this the two tables interleave.
    frame.render_widget(Clear, area);

    let title = Line::from(vec![
        Span::styled(format!(" FILES  {} ", files.folder), theme::label()),
        Span::styled(
            format!("· {} · {} ", files.peer, plural(files.files.len(), "file")),
            theme::muted(),
        ),
    ]);

    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("file"),
        Cell::from("size"),
    ])
    .style(theme::label());
    let rows: Vec<Row> = files
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            Row::new(vec![
                Cell::from((index + 1).to_string()),
                Cell::from(crate::out::terminal_safe(&basename(&file.path))),
                Cell::from(file.size.to_string()),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Min(24),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .column_spacing(1)
    .row_highlight_style(theme::selected())
    .highlight_symbol("▸ ")
    .block(Block::bordered().title(title));
    let mut state = TableState::default().with_selected(Some(files.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn help_overlay(frame: &mut Frame, area: Rect) {
    let overlay = centered(area, area.width.saturating_sub(8).min(72), 15);
    frame.render_widget(Clear, overlay);

    let key = |keys: &str, meaning: &'static str| {
        Line::from(vec![
            Span::styled(format!("  {keys:<12}"), theme::accent()),
            Span::raw(meaning),
        ])
    };

    let text = vec![
        Line::from(Span::styled("  Keys", theme::label())),
        Line::default(),
        key("/", "search for a release"),
        key("Enter", "run the typed search"),
        key("Tab", "switch between Sources and Discogs"),
        key("j / k", "move the selection"),
        key("g / G", "first / last row"),
        key("Enter, l", "list the files in a folder"),
        key("d", "download the highlighted folder"),
        key("t", "use the highlighted Discogs pressing"),
        key("b, s", "rate budgets, recent searches"),
        key("r", "reload the budgets or sessions list"),
        key("Esc, h", "back"),
        key("Ctrl-C", "quit"),
    ];

    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(Span::styled(" help ", theme::wordmark()))),
        overlay,
    );
}

fn footer(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([
        Constraint::Length(u16::from(app.notice.is_some())),
        Constraint::Length(1),
    ])
    .split(area);

    if let Some(notice) = &app.notice {
        let style = match notice.level {
            Level::Info => theme::accent(),
            Level::Error => theme::bad(),
        };
        let marker = match notice.level {
            Level::Info => "·",
            Level::Error => "✗",
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {marker} "), style),
                Span::styled(notice.text.clone(), style),
            ])),
            rows[0],
        );
    }

    let hints = if app.files.is_some() {
        "j/k move   Esc, h back   Ctrl-C quit"
    } else {
        match app.screen {
            Screen::Setup => "Tab switch fields   Enter save   Ctrl-C quit",
            Screen::Home => "/ search   b budgets   s sessions   ? help   Ctrl-C quit",
            Screen::Results => match app.focus {
                Focus::Sources => {
                    "Tab switch   j/k move   d download   Enter files   / search   b budgets   ? help   Ctrl-C quit"
                }
                Focus::Discogs => {
                    "Tab switch   j/k move   Enter use this pressing   b budgets   ? help   Ctrl-C quit"
                }
            },
            Screen::Download => "j/k scroll   Esc, h back   Ctrl-C quit",
            Screen::Budgets | Screen::Sessions => "r refresh   Esc, h back   ? help   Ctrl-C quit",
            Screen::Help => "Esc, h close help   Ctrl-C quit",
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {hints} "),
            theme::muted(),
        ))),
        rows[1],
    );
}

fn phase_label(phase: SearchPhase) -> &'static str {
    match phase {
        SearchPhase::Discogs => "asking Discogs…",
        SearchPhase::SoulSeek => "listening for peers…",
    }
}

fn section_title(left: &str, right: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {left} "), style.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {right} "), theme::muted()),
    ])
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}

/// `1 folder` / `3 folders`. A count of one is common enough to be worth
/// getting right.
fn plural(count: usize, singular: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {singular}s")
    }
}

/// How many cells of a `width`-cell bar are filled.
///
/// Integer arithmetic, so a bar cannot be a row of cells off and no float
/// rounding can drop the last cell.
fn fill(available: u32, capacity: u32, width: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    let capacity = usize::try_from(capacity).unwrap_or(usize::MAX);
    usize::try_from(available)
        .unwrap_or(usize::MAX)
        .saturating_mul(width)
        .checked_div(capacity)
        .unwrap_or(0)
        .min(width)
}

/// Shortens a long single-line message so it cannot push the status off screen.
fn head(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut shortened: String = text.chars().take(max.saturating_sub(1)).collect();
    shortened.push('…');
    shortened
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::time::Duration;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::actions::download::DownloadEvent;
    use crate::actions::search::SoulSeekFound;
    use crate::tui::app::{App, ConfigSummary, DownloadView, FilesView};
    use crate::tui::fixtures as fx;

    /// Renders the whole interface headlessly and returns its text, so a layout
    /// regression is caught without a terminal.
    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| draw(frame, app))
            .expect("the frame draws");

        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn a_budget_bar_fills_to_scale_and_never_overflows() {
        assert_eq!(fill(34, 34, 20), 20);
        assert_eq!(fill(17, 34, 20), 10);
        assert_eq!(fill(0, 34, 20), 0);
        assert_eq!(fill(1, 0, 20), 0, "a zero capacity divided");
        assert!(fill(u32::MAX, 1, 20) <= 20, "the bar overflowed its width");
    }

    #[test]
    fn the_home_screen_names_the_app_and_shows_what_is_configured() {
        let mut app = App::new(fx::config(), None);
        let text = screen(&mut app, 80, 24);

        // The wordmark is block art here, not spaced capitals.
        assert!(text.contains('█'), "the wordmark art is missing: {text}");
        assert!(!text.contains("A L D O"), "{text}");
        assert!(text.contains("Artist - Album"), "{text}");
        assert!(text.contains("Discogs"), "{text}");
        assert!(text.contains("/home/you/Music"), "{text}");
        assert!(text.contains("/ search"), "{text}");

        // The search box is three rows, not one. Adding a row to the layout once
        // shifted every index after it and collapsed the box to its top border,
        // which none of the assertions above noticed.
        assert!(
            text.contains("press / to search"),
            "the search box lost its prompt: {text}"
        );
        assert!(
            text.contains('┐') && text.contains('└'),
            "the search box lost a border: {text}"
        );
    }

    #[test]
    fn a_terminal_too_small_for_the_art_gets_the_plain_wordmark() {
        // Clipping a logo is worse than not showing one.
        let mut app = App::new(fx::config(), None);
        let text = screen(&mut app, 24, 12);

        assert!(text.contains("A L D O"), "{text}");
        assert!(
            !text.contains('█'),
            "the art was squeezed into 24 columns: {text}"
        );
    }

    #[test]
    fn the_setup_screen_asks_for_credentials_and_hides_the_secrets() {
        let mut app = App::new(fx::needs_setup(), None);
        app.setup.username.set("alice");
        app.setup.password.set("hunter2");
        let text = screen(&mut app, 90, 24);

        assert!(text.contains("SoulSeek user"), "{text}");
        assert!(text.contains("alice"), "{text}");
        assert!(text.contains('•'), "the password is not masked: {text}");
        assert!(!text.contains("hunter2"), "the password leaked: {text}");
        assert!(text.contains("aldo logout"), "{text}");
    }

    #[test]
    fn a_missing_provider_is_marked_rather_than_left_to_be_discovered() {
        let mut app = App::new(
            ConfigSummary {
                library_root: PathBuf::from("/music"),
                discogs: false,
                server: false,
            },
            None,
        );
        // A fresh install starts on the sign-in form; this test is about the
        // home screen's chips, so it jumps there directly.
        app.screen = Screen::Home;
        let text = screen(&mut app, 80, 24);
        assert!(text.contains("✗ Discogs"), "{text}");
        assert!(text.contains("✗ SoulSeek"), "{text}");
    }

    #[test]
    fn the_results_screen_lists_the_folders_and_the_release() {
        let mut app = fx::with_results();
        let text = screen(&mut app, 120, 30);

        assert!(text.contains("Surf's Up"), "{text}");
        assert!(text.contains("The Beach Boys"), "{text}");
        assert!(text.contains("alice"), "{text}");
        assert!(text.contains("44 kHz"), "{text}");
        assert!(text.contains("free"), "{text}");
        assert!(text.contains("26707685"), "{text}");
        assert!(text.contains("d download"), "{text}");
    }

    #[test]
    fn a_search_error_is_shown_in_the_header_not_swallowed() {
        let mut app = fx::app();
        app.screen = Screen::Results;
        app.error = Some("the network is unreachable".to_owned());
        let text = screen(&mut app, 100, 20);

        assert!(text.contains("unreachable"), "{text}");
    }

    #[test]
    fn the_files_overlay_names_the_folder_and_counts_the_files() {
        let mut app = fx::with_results();
        let source = fx::source();
        app.files = Some(FilesView {
            peer: "alice".to_owned(),
            folder: "The Beach Boys/Surf's Up".to_owned(),
            files: source.files,
            selected: 0,
        });
        let text = screen(&mut app, 100, 24);

        assert!(text.contains("Surf's Up"), "{text}");
        assert!(text.contains("2 files"), "{text}");
        assert!(text.contains("Don't Go Near the Water"), "{text}");
        // The footer must describe the panel that is on screen, not the one
        // behind it.
        assert!(text.contains("Esc, h back"), "{text}");
        assert!(!text.contains("d download"), "{text}");
    }

    #[test]
    fn the_download_screen_shows_both_bars_and_every_file() {
        let mut app = fx::app();
        let plan = fx::plan();
        let mut view = DownloadView::new(&plan);
        view.apply(&DownloadEvent::FileStarted {
            index: 1,
            total: 2,
            name: "a.flac".to_owned(),
            size: ByteSize::new(30_000_000),
        });
        view.apply(&DownloadEvent::Progress {
            bytes_done: 15_000_000,
            bytes_total: 30_000_000,
            speed_bps: 1_048_576,
            queued: false,
        });
        let _ = view.done_bytes();
        app.download = Some(view);
        app.screen = Screen::Download;

        let text = screen(&mut app, 110, 26);
        assert!(text.contains("DOWNLOAD"), "{text}");
        assert!(text.contains("The Beach Boys - Surf's Up"), "{text}");
        assert!(text.contains("Long Promised Road"), "{text}");
        assert!(text.contains("1.0 MiB/s"), "{text}");
    }

    #[test]
    fn the_confirmation_says_where_the_files_will_land() {
        let mut app = fx::with_results();
        app.confirm = Some(Confirm {
            prepared: fx::prepared(),
            library_root: PathBuf::from("/home/you/Music"),
        });
        let text = screen(&mut app, 100, 26);

        assert!(text.contains("download?"), "{text}");
        assert!(text.contains("The Beach Boys - Surf's Up"), "{text}");
        assert!(
            text.contains("/home/you/Music/The Beach Boys/Surf's Up"),
            "the exact destination is not shown: {text}"
        );
        assert!(text.contains("release 26707685"), "{text}");
        assert!(text.contains("Esc to cancel"), "{text}");
    }

    #[test]
    fn the_help_overlay_documents_the_keys() {
        let mut app = fx::app();
        app.screen = Screen::Help;
        let text = screen(&mut app, 80, 24);

        assert!(text.contains("help"), "{text}");
        assert!(
            text.contains("switch between Sources and Discogs"),
            "{text}"
        );
        assert!(text.contains("download the highlighted folder"), "{text}");
        assert!(text.contains("quit"), "{text}");
    }

    #[test]
    fn an_empty_folder_list_is_explained_in_place() {
        let mut app = fx::with_results();
        if let Some(run) = app.search.as_mut() {
            run.soulseek = SoulSeekReport::Answered(SoulSeekFound {
                candidates: Vec::new(),
                raw_hits: 0,
                rejected: 0,
                shares_anything: false,
                shared_directories: vec!["/home/you/Music".to_owned()],
                search_timeout: Duration::from_secs(8),
                listen_port: 2234,
            });
        }
        let text = screen(&mut app, 110, 24);
        assert!(text.contains("sharing nothing"), "{text}");
    }

    #[test]
    fn a_select_all_says_what_will_clear_it() {
        let mut app = fx::app();
        app.input_open = true;
        app.input.set("bad brains");
        app.input.select_all();

        let text = screen(&mut app, 80, 24);
        assert!(text.contains("bad brains"), "{text}");
        assert!(
            text.contains("Backspace or Delete clears the query"),
            "the selection is invisible: {text}"
        );
    }

    #[test]
    fn a_narrow_terminal_still_draws_every_screen_without_panicking() {
        // A resize to something tiny is common when a multiplexer pane is
        // dragged, and it must not take the process down.
        for screen_name in [
            Screen::Setup,
            Screen::Home,
            Screen::Results,
            Screen::Download,
            Screen::Budgets,
            Screen::Sessions,
            Screen::Help,
        ] {
            let mut app = fx::with_results();
            app.download = Some(DownloadView::new(&fx::plan()));
            app.confirm = Some(Confirm {
                prepared: fx::prepared(),
                library_root: PathBuf::from("/home/you/Music"),
            });
            app.screen = screen_name;
            let text = screen(&mut app, 12, 5);
            assert!(!text.is_empty(), "{screen_name:?} drew nothing");
        }
    }
}
