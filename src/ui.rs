//! Rendering the Changes view: tab bar, file list, diff, comment box, list, status.
//!
//! See `specs/tui.md`. The layout is a header tab bar, a body split into the read pane
//! and navigator, and a status bar. While composing, the comment
//! box is spliced inline into the diff under the selected line; the comments-list
//! overlay is drawn on top when open. Rendering reads `App` only; all state changes
//! live in `app.rs`.

use std::rc::Rc;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Band, Focus, FooterAction, Mode, Tab};
use crate::config::NavigatorPosition;
use crate::diff::{FileDiff, FileState, Row};
use crate::file_list::{Annotation, RowKind};
use crate::forge;
use crate::herdr::AgentChoice;
use crate::keymap::Keymap;
use crate::model::Comment;
use crate::theme::Palette;

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // Link hit-testing resolves against the painted frame; each frame repaints its own.
    app.clear_painted_links();
    if let Some(error) = app.config_error() {
        let message =
            format!("{error}\n\nFix the file to continue. The config reloads automatically.");
        frame.render_widget(
            Paragraph::new(message).wrap(ratatui::widgets::Wrap { trim: false }),
            area,
        );
        return;
    }
    let p = panes(area, app);

    if app.tab == Tab::Issues {
        render_pr_header(frame, app, p.tab);
        render_issues_read(frame, app, p.diff);
        render_issues_nav(frame, app, p.files);
    } else if app.tab == Tab::Pr {
        render_pr_header(frame, app, p.tab);
        render_pr_read(frame, app, p.diff);
        // `PR` never hides its navigator (specs/tui.md), so no hidden gate here.
        render_pr_nav(frame, app, p.files);
    } else {
        render_tab_bar(frame, app, p.tab);
        render_diff_view(frame, app, p.diff);
        if !app.navigator_hidden_here() {
            render_file_list(frame, app, p.files);
        }
    }
    // One footer band on every tab, drawn after the per-tab base so it sits on both layouts.
    render_footer(frame, app, p.status);

    // A popup paints last, over the page it scrims. One match decides both the scrim and what
    // paints, so a mode can never scrim the page and then draw nothing. Both popups place through
    // `body_popup`, so the footer just drawn stays uncovered and keeps advertising their keys.
    let popup: Option<fn(&mut Frame, &App, Rect)> = match app.mode {
        Mode::List => Some(render_comments_list),
        Mode::Picker => Some(render_agent_picker),
        Mode::BasePick => Some(render_base_picker),
        Mode::Normal | Mode::Composing { .. } | Mode::Find => None,
    };
    if let Some(render_popup) = popup {
        scrim_behind(frame, app, area);
        render_popup(frame, app, area);
    }
}

/// Recede everything behind an open modal, except the footer: every painted color in the tab
/// bar and the body blends halfway to the theme base, so the modal owns the eye while the page
/// stays recognizable. The footer stays bright — while a modal is open it is the modal's own
/// key bar, the one place advertising the live keys (`specs/tui.md`, `specs/input.md`).
fn scrim_behind(frame: &mut Frame, app: &App, area: Rect) {
    let p = *app.palette();
    let bands = panes(area, app);
    let buf = frame.buffer_mut();
    for band in [bands.tab, bands.body] {
        for y in band.y..band.y + band.height {
            for x in band.x..band.x + band.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.fg = p.scrim(cell.fg);
                    cell.bg = p.scrim(cell.bg);
                }
            }
        }
    }
}

/// The vertical bands: tab bar, body, footer. The comment input is inline in the diff, not a band
/// of its own. The footer is one row until the `?` expansion opens it, when it grows by the wrapped
/// bands — capped so the body keeps its `Min(3)` (`specs/input.md`, `tui.md`).
fn vrows(area: Rect, app: &App) -> Rc<[Rect]> {
    let footer = footer_height(app, area);
    Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(footer)])
        .split(area)
}

/// The frame's layout rects: the read pane, the navigator, and the whole body band. One
/// place computes the vertical bands and the active split, so every geometry helper and
/// the renderer agree by construction (a layout change can't desync hit-testing from paint).
struct Panes {
    tab: Rect,
    diff: Rect,
    files: Rect,
    body: Rect,
    status: Rect,
}

fn panes(area: Rect, app: &App) -> Panes {
    let rows = vrows(area, app);
    let body = rows[1];
    // A hidden navigator gives the read pane the whole body. The zero-sized files rect keeps
    // every hit-test missing it by construction (`specs/tui.md`).
    let (diff, files) = if app.navigator_hidden_here() {
        (body, Rect::new(body.x, body.y, 0, 0))
    } else {
        split_body(body, app.navigator_position, app.navigator_share())
    };
    Panes { tab: rows[0], diff, files, body, status: rows[2] }
}

/// Split `axis_len` cells by `pct`, honoring the shared minimum-pane rule: a three-cell
/// floor for each side once six cells exist, an even split below (`specs/tui.md`). The one
/// home for the review split's minimum.
pub(crate) fn split_axis(axis_len: u16, pct: u16) -> u16 {
    let mut len = (u32::from(axis_len) * u32::from(pct) / 100) as u16;
    if axis_len >= 6 {
        len = len.clamp(3, axis_len - 3);
    } else {
        len = axis_len / 2;
    }
    len
}

fn split_body(body: Rect, position: NavigatorPosition, share: u16) -> (Rect, Rect) {
    let axis_len = if position.stacked() { body.height } else { body.width };
    let navigator_len = split_axis(axis_len, share);
    let read_len = axis_len - navigator_len;
    match position {
        NavigatorPosition::Right => (
            Rect::new(body.x, body.y, read_len, body.height),
            Rect::new(body.x + read_len, body.y, navigator_len, body.height),
        ),
        NavigatorPosition::Left => (
            Rect::new(body.x + navigator_len, body.y, read_len, body.height),
            Rect::new(body.x, body.y, navigator_len, body.height),
        ),
        NavigatorPosition::Bottom => (
            Rect::new(body.x, body.y, body.width, read_len),
            Rect::new(body.x, body.y + read_len, body.width, navigator_len),
        ),
        NavigatorPosition::Top => (
            Rect::new(body.x, body.y + navigator_len, body.width, read_len),
            Rect::new(body.x, body.y, body.width, navigator_len),
        ),
    }
}

/// The whole body band (between the tab bar and status bar), for divider hit-testing.
#[must_use]
pub fn body_rect(area: Rect, app: &App) -> Rect {
    vrows(area, app)[1]
}

/// Whether `(col, row)` lands on the draggable divider between the two panes.
#[must_use]
pub fn hit_divider(area: Rect, app: &App, col: u16, row: u16) -> bool {
    // No divider exists while the navigator is hidden — the zero-sized files rect would
    // otherwise still seam-match at the body's edge (`specs/tui.md`).
    if app.navigator_hidden_here() {
        return false;
    }
    let p = panes(area, app);
    match app.navigator_position {
        NavigatorPosition::Left => {
            contains(p.body, col, row) && at_seam(col, p.files.x + p.files.width)
        }
        NavigatorPosition::Right => contains(p.body, col, row) && at_seam(col, p.files.x),
        NavigatorPosition::Top => {
            contains(p.body, col, row) && at_seam(row, p.files.y + p.files.height)
        }
        NavigatorPosition::Bottom => contains(p.body, col, row) && at_seam(row, p.files.y),
    }
}

/// The two adjacent pane-border cells around a split boundary.
fn at_seam(coordinate: u16, boundary: u16) -> bool {
    coordinate == boundary || coordinate.checked_add(1) == Some(boundary)
}

/// The file-row index a click at `(col, row)` lands on, or `None` if outside the list.
/// `file_scroll` is the top visible row, so a click maps to the scrolled-to row.
#[must_use]
pub fn hit_file(
    area: Rect,
    app: &App,
    col: u16,
    row: u16,
    n_files: usize,
    file_scroll: usize,
) -> Option<usize> {
    let inner = inner_rect(panes(area, app).files);
    if !contains(inner, col, row) {
        return None;
    }
    let idx = (row - inner.y) as usize + file_scroll;
    (idx < n_files).then_some(idx)
}

/// The number of file rows visible in the file pane, used to clamp the file-list scroll.
#[must_use]
pub fn file_viewport_height(area: Rect, app: &App) -> usize {
    inner_rect(panes(area, app).files).height as usize
}

/// Whether `(col, row)` falls in the file pane, so the wheel scrolls the list it is over.
#[must_use]
pub fn in_files_pane(area: Rect, app: &App, col: u16, row: u16) -> bool {
    contains(panes(area, app).files, col, row)
}

/// Whether `(col, row)` falls in the diff pane — the markdown preview's click target,
/// whose rendered geometry the source-row hit test cannot describe.
#[must_use]
pub fn in_diff_pane(area: Rect, app: &App, col: u16, row: u16) -> bool {
    contains(panes(area, app).diff, col, row)
}

/// The logical diff-row index a click at `(col, row)` lands on, or `None` if outside the
/// diff pane. `heights` (display rows per logical row) and `diff_scroll` reproduce the
/// painted window, so a click on any display line of a wrapped row maps to that row.
#[must_use]
pub fn hit_diff(
    area: Rect,
    app: &App,
    col: u16,
    row: u16,
    heights: &[usize],
    diff_scroll: usize,
) -> Option<usize> {
    let inner = inner_rect(panes(area, app).diff);
    if !contains(inner, col, row) {
        return None;
    }
    let target = (row - inner.y) as usize;
    let mut acc = 0;
    for (li, h) in heights.iter().enumerate().skip(diff_scroll) {
        acc += h;
        if target < acc {
            return Some(li);
        }
    }
    None
}

/// The number of diff rows visible in the diff pane, used to clamp the scroll.
#[must_use]
pub fn diff_viewport_height(area: Rect, app: &App) -> usize {
    let h = inner_rect(panes(area, app).diff).height as usize;
    // The find band takes the pane's bottom row, so the cursor reveals above it
    // (specs/find-in-file.md).
    if app.mode == crate::app::Mode::Find { h.saturating_sub(1) } else { h }
}

/// The display height (rows on screen) of each visible logical diff row, honoring wrap.
#[must_use]
pub fn diff_row_heights(app: &App, area: Rect) -> Vec<usize> {
    let width = inner_rect(panes(area, app).diff).width as usize;
    let gutter_w = gutter_for(&app.diff);
    let p = app.palette();
    // A row's display height is its wrapped code lines plus any inline comment cards under
    // it (excluding a card whose comment is being edited), so scroll-clamping and hit-testing
    // match what the renderer paints.
    let cards = app.comment_cards();
    let editing = editing_comment(app);
    app.visible
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let base = row_height(r, gutter_w, width, app.wrap);
            let card: usize = cards[i]
                .iter()
                .filter(|&&ci| Some(ci) != editing)
                .filter_map(|&ci| app.store.get(ci))
                .map(|c| comment_card_lines(c, width, p).len())
                .sum();
            base + card
        })
        .collect()
}

/// The store index of the comment currently being edited, whose inline card is hidden in
/// favor of its edit box; `None` when not editing.
fn editing_comment(app: &App) -> Option<usize> {
    match app.mode {
        Mode::Composing { editing } => editing,
        _ => None,
    }
}

/// Rows the inline comment box occupies at the diff pane's `width`: the wrapped body height
/// (so the box grows as text wraps, not only on explicit newlines) plus the two borders.
#[must_use]
pub fn composer_height(app: &App, width: usize) -> usize {
    box_rows(&app.input, composer_content_width(width)).len() + 2
}

/// The text width inside the comment box: the diff pane width minus its two borders.
#[must_use]
pub fn composer_content_width(width: usize) -> usize {
    width.saturating_sub(2).max(1)
}

/// The diff pane's inner content width for the full terminal `area`, so the event loop can
/// reserve the comment box without a `Frame` (mirrors [`diff_viewport_height`]).
#[must_use]
pub fn diff_inner_width(area: Rect, app: &App) -> usize {
    inner_rect(panes(area, app).diff).width as usize
}

/// The comment box's display lines at `content_w`: each input line word-wrapped, with the
/// caret drawn as a block at its mapped (row, column). An empty box shows a placeholder.
fn composer_lines(app: &App, content_w: usize) -> Vec<Line<'static>> {
    let p = app.palette();
    if app.input.is_empty() {
        return vec![Line::from(vec![
            Span::styled(" ", caret_style(p)),
            Span::styled("Leave a comment…", Style::default().fg(p.overlay0)),
        ])];
    }
    let rows = box_rows(&app.input, content_w);
    let (caret_row, caret_col) = caret_rowcol(&rows, app.caret);
    rows.iter()
        .enumerate()
        .map(|(i, (_, text))| {
            if i == caret_row {
                row_with_caret(text, caret_col, p)
            } else {
                Line::from(text.clone())
            }
        })
        .collect()
}

/// The block-cursor style: the character under the caret shown dark-on-peach.
fn caret_style(p: &Palette) -> Style {
    Style::default().fg(p.surface0).bg(p.peach)
}

/// One box row with the caret block over the character at `col` (a trailing block at the end).
fn row_with_caret(text: &str, col: usize, p: &Palette) -> Line<'static> {
    let chars: Vec<char> = text.chars().collect();
    let col = col.min(chars.len());
    let left: String = chars[..col].iter().collect();
    let mut spans = vec![Span::raw(left)];
    if col < chars.len() {
        spans.push(Span::styled(chars[col].to_string(), caret_style(p)));
        spans.push(Span::raw(chars[col + 1..].iter().collect::<String>()));
    } else {
        spans.push(Span::styled(" ".to_string(), caret_style(p)));
    }
    Line::from(spans)
}

/// The box's visual rows over the whole `input`: `(start_char_index, text)` per row, wrapping
/// each logical line with [`wrap_segments`]. A trailing newline yields an empty row.
fn box_rows(input: &str, width: usize) -> Vec<(usize, String)> {
    let chars: Vec<char> = input.chars().collect();
    let mut rows = Vec::new();
    let mut i = 0;
    loop {
        let line_end = chars[i..].iter().position(|&c| c == '\n').map_or(chars.len(), |p| i + p);
        let cells: Vec<Cell> = chars[i..line_end].iter().copied().map(plain_cell).collect();
        for (a, b) in wrap_segments(&cells, width, ContinuationSpaces::Keep) {
            rows.push((i + a, chars[i + a..i + b].iter().collect::<String>()));
        }
        match chars[line_end..].first() {
            Some('\n') => {
                i = line_end + 1;
                if i == chars.len() {
                    rows.push((i, String::new())); // a trailing newline opens an empty row
                    break;
                }
            }
            _ => break,
        }
    }
    if rows.is_empty() {
        rows.push((0, String::new()));
    }
    rows
}

/// Map a caret char index to its `(row, col)` in the box rows: the last row that starts at or
/// before the caret, with the column clamped to that row's length.
fn caret_rowcol(rows: &[(usize, String)], caret: usize) -> (usize, usize) {
    let row = rows.iter().rposition(|(start, _)| *start <= caret).unwrap_or(0);
    let (start, text) = &rows[row];
    (row, (caret - start).min(text.chars().count()))
}

/// The new caret char index after moving up (`down == false`) or down one wrapped row, keeping
/// the column where the target row allows. For `↑`/`↓` in the comment editor.
#[must_use]
pub fn caret_vertical(input: &str, caret: usize, content_w: usize, down: bool) -> usize {
    let rows = box_rows(input, content_w);
    let (row, col) = caret_rowcol(&rows, caret);
    let target = if down { (row + 1).min(rows.len() - 1) } else { row.saturating_sub(1) };
    let (start, text) = &rows[target];
    start + col.min(text.chars().count())
}

/// Word-wrap a plain string to `width` columns, reusing the diff's [`wrap_segments`] so the
/// break rule (last space, hard-break an over-wide word, width-aware) is identical.
fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let cells: Vec<Cell> = s.chars().map(plain_cell).collect();
    wrap_segments(&cells, width, ContinuationSpaces::Trim)
        .into_iter()
        .map(|(a, b)| cells[a..b].iter().map(|c| c.ch).collect())
        .collect()
}

/// A clickable region in the header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeaderHit {
    Tab(Tab),
    Scope,
    /// The `branch` scope's base label; the click opens the base picker (`specs/tui.md`).
    Base,
}

/// Which header control a click at `(col, row)` lands on, if any. `keymap` must be the keymap
/// the on-screen frame was drawn with, so a config swap between the draw and the click cannot
/// shift the spans under the pointer (`specs/config.md`: one snapshot per frame).
#[must_use]
pub fn hit_header(area: Rect, app: &App, keymap: &Keymap, col: u16, row: u16) -> Option<HeaderHit> {
    if row != area.y {
        return None;
    }
    let spans = tab_spans(keymap, tab_cap(app, area.width));
    for &(tab, start, end) in &spans {
        if (start as u16..end as u16).contains(&col) {
            return Some(HeaderHit::Tab(tab));
        }
    }
    let prefix = header_prefix_len(&spans);
    let scope_start = prefix as u16;
    let scope_end = scope_start + scope_chip(app).len() as u16;
    if (scope_start..scope_end).contains(&col) {
        return Some(HeaderHit::Scope);
    }
    if let Some((lead, name, tail)) = base_parts(app, keymap, area.width) {
        let base_start = scope_end + BASE_GAP.len() as u16;
        let base_end = base_start + (lead.width() + name.width() + tail.width()) as u16;
        if (base_start..base_end).contains(&col) {
            return Some(HeaderHit::Base);
        }
    }
    None
}

/// The three tabs and their labels, left to right, each led by its `tab-*` action's hint key
/// (`specs/input.md`). Column math uses display width, since a bound hint key can be wide.
fn tab_labels(keymap: &Keymap) -> [(Tab, String); 3] {
    use crate::keymap::Action as K;
    [
        (Tab::Changes, format!("{} Changes", keymap.hint(K::TabChanges))),
        (Tab::Pr, format!("{} PR", keymap.hint(K::TabPr))),
        (Tab::Issues, format!("{} Issues", keymap.hint(K::TabIssues))),
    ]
}
const HEADER_LEAD: &str = " ";
const TAB_GAP: &str = "  ";
const HEADER_GAP: &str = "  ";
/// The gap between the scope chip and the base label — one spelling shared by the paint,
/// the width math, and the click hit-test, so the painted text and the clickable region
/// can never drift apart.
const BASE_GAP: &str = " ";
/// The reserved indicator cell at the end of the tab strip: one gap column plus one glyph
/// column, always present so nothing shifts when the glyph appears (specs/tui.md).
const INDICATOR_CELL: usize = 2;

/// The reserved cell's content: the refresh glyph while the active tab's refresh has been
/// in flight past the delay, a blank cell otherwise.
fn indicator_glyph(app: &App) -> &'static str {
    if app.refresh_indicator { "⟳" } else { " " }
}

/// Each tab's `(tab, start_col, end_col)` in the header, the single source the bar paints and
/// the click hit-tests against. `cap` is the label cap the strip is painted with, so a
/// shortened strip is clicked where it reads, not where whole labels would have been.
fn tab_spans(keymap: &Keymap, cap: Option<usize>) -> Vec<(Tab, usize, usize)> {
    let mut col = HEADER_LEAD.len();
    let mut out = Vec::new();
    for (i, (tab, label)) in tab_labels(keymap).iter().enumerate() {
        let label = match cap {
            Some(max) => truncate_width(label, max),
            None => label.clone(),
        };
        if i > 0 {
            col += TAB_GAP.len();
        }
        out.push((*tab, col, col + label.width()));
        col += label.width();
    }
    out
}

/// The column where the scope chip starts: past the tab bar, its reserved spinner cell,
/// and its trailing gap.
fn header_prefix_len(spans: &[(Tab, usize, usize)]) -> usize {
    spans.last().map_or(HEADER_LEAD.len(), |&(_, _, end)| end) + INDICATOR_CELL + HEADER_GAP.len()
}

fn scope_chip(app: &App) -> String {
    format!("[{}]", app.scope.label())
}

/// The `branch` scope's base label as `(lead, name, tail)`: `vs ` + the bare name however it
/// resolved, or the empty-state `no base` in the name slot with no lead. The skipped-choice
/// tail ` · <name> missing` follows either, so a dormant choice never reads as never-chosen
/// (`specs/tui.md`). `None` off the `branch` scope.
fn base_label(app: &App) -> Option<(String, String, String)> {
    if app.scope != crate::model::Scope::Branch {
        return None;
    }
    let tail = match &app.branch_base.skipped {
        Some(missing) => format!(" · {missing} missing"),
        None => String::new(),
    };
    Some(match &app.branch_base.winner {
        Some(b) => ("vs ".to_string(), b.name.clone(), tail),
        None => (String::new(), "no base".to_string(), tail),
    })
}

/// The base label as painted: truncated with a trailing `…` to what the header can fit,
/// the name first and the skipped tail only in what remains, so a long missing name can
/// never evict the resolved base (`specs/tui.md`) — one source for the paint and the
/// click hit-test.
fn base_parts(app: &App, keymap: &Keymap, width: u16) -> Option<(String, String, String)> {
    let (lead, name, tail) = base_label(app)?;
    // Everything else on the line plus the base's own gap and the suffix's minimum gap.
    let fixed = header_prefix_len(&tab_spans(keymap, tab_cap(app, width)))
        + scope_chip(app).len()
        + BASE_GAP.len()
        + lead.width()
        + header_suffix(app).width()
        + HEADER_LEAD.len()
        + 1;
    let budget = (width as usize).saturating_sub(fixed);
    let name = truncate_width(&name, budget);
    if name.is_empty() {
        // Not even one column for the name: the base leaves the header whole rather than
        // paint a nameless `vs` the click would still claim (`specs/tui.md`).
        return None;
    }
    let tail = truncate_width(&tail, budget.saturating_sub(name.width()));
    Some((lead, name, tail))
}

/// The header suffix: the active scope's changed-file count and its aggregate line totals, in
/// [`stats_str`]'s grammar, so a zero side drops and an empty changeset shows the bare count.
/// The totals' `−` is multi-byte, so the suffix is measured by display width; the scope chip
/// is all-ASCII, so its byte `.len()` equals its display width.
fn header_suffix(app: &App) -> String {
    let (added, removed) = app.changed_totals();
    let stats = stats_str(added, removed);
    let gap = if stats.is_empty() { "" } else { "  " };
    format!("{} changed{gap}{stats}", app.changed_count())
}

/// The header's shared left side, painted by both tab bars: the lead pad, the three tab labels
/// (the active one bright + underlined, the inactive ones at `SUBTEXT0`), and the trailing gap
/// before each header's own suffix. One source so the two headers can't drift.
fn tab_bar_spans(app: &App) -> Vec<Span<'static>> {
    tab_bar_spans_capped(app, None)
}

/// The columns the strip may claim on a header of `width`.
///
/// Each header has one element that outlives everything else: the PR's identity chip on a
/// forge tab (`specs/pr-tab.md`), the scope chip on `Changes` (`specs/tui.md`). The strip
/// yields to it. Without this a third tab pushes that element off a narrow bar entirely,
/// which is the one thing the narrow-bar rule promises will not happen.
fn tab_strip_budget(app: &App, width: u16) -> usize {
    let reserved = if app.tab.is_forge() {
        match &app.pr {
            forge::PrView::Pr(s) => pr_chip_width(app, s) + 2,
            _ => 0,
        }
    } else {
        scope_chip(app).len() + HEADER_LEAD.len()
    };
    (width as usize).saturating_sub(reserved)
}

/// The label cap the strip is painted with on a header of `width`, or `None` while the whole
/// labels fit. One source for the paint and the click hit-test, so they cannot disagree about
/// where a shortened tab sits.
fn tab_cap(app: &App, width: u16) -> Option<usize> {
    let budget = tab_strip_budget(app, width);
    if tab_bar_spans(app).iter().map(Span::width).sum::<usize>() <= budget {
        return None;
    }
    // Shrink every label by the same cap so the strip stays a readable row of equals rather
    // than one full label beside stubs. The floor keeps each tab's hint key legible.
    Some(
        (3..=12)
            .rev()
            .find(|&cap| {
                tab_bar_spans_capped(app, Some(cap)).iter().map(Span::width).sum::<usize>()
                    <= budget
            })
            .unwrap_or(3),
    )
}

fn tab_bar_spans_capped(app: &App, cap: Option<usize>) -> Vec<Span<'static>> {
    let p = app.palette();
    let bar = Style::default().bg(p.surface0);
    let mut spans = vec![Span::styled(HEADER_LEAD, bar)];
    for (i, (tab, label)) in tab_labels(app.keymap()).into_iter().enumerate() {
        let label = match cap {
            Some(max) => truncate_width(&label, max),
            None => label,
        };
        if i > 0 {
            spans.push(Span::styled(TAB_GAP, bar));
        }
        let style = if tab == app.tab {
            bar.fg(p.lavender).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            bar.fg(p.subtext0)
        };
        spans.push(Span::styled(label, style));
    }
    // The reserved indicator cell (specs/tui.md): blank when idle, so nothing shifts.
    spans.push(Span::styled(" ", bar));
    // Quiet like the header's secondary text — status, not an alert (specs/tui.md).
    spans.push(Span::styled(indicator_glyph(app), bar.fg(p.overlay0)));
    spans.push(Span::styled(HEADER_GAP, bar));
    spans
}

fn render_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let chip = scope_chip(app);
    let base = base_parts(app, app.keymap(), area.width);
    let base_width = base.as_ref().map_or(0, |(lead, name, tail)| {
        BASE_GAP.len() + lead.width() + name.width() + tail.width()
    });
    let suffix = header_suffix(app);
    let cap = tab_cap(app, area.width);
    let prefix = header_prefix_len(&tab_spans(app.keymap(), cap));
    // The suffix keeps the same edge pad as the tab strip's lead.
    let used = prefix + chip.len() + base_width + suffix.width() + HEADER_LEAD.len();
    // Right-align the suffix; at least one gap column when the bar overflows.
    let pad = (area.width as usize).saturating_sub(used).max(1);

    // A quiet surface bar: the active tab in bright lavender, the inactive one dimmed, the
    // clickable scope control accented so it reads as a button.
    let p = app.palette();
    let bar = Style::default().bg(p.surface0);
    let mut spans = tab_bar_spans_capped(app, cap);
    spans.push(Span::styled(chip, bar.fg(p.yellow).add_modifier(Modifier::BOLD)));
    if let Some((lead, name, tail)) = base {
        // An empty lead is the `no base` state, worn as a warning; a resolved name wears the
        // clickable accent, and the skipped tail warns beside it (`specs/tui.md`).
        let warn = lead.is_empty();
        spans.push(Span::styled(BASE_GAP, bar));
        spans.push(Span::styled(lead, bar.fg(p.overlay0)));
        spans.push(Span::styled(name, bar.fg(if warn { p.peach } else { p.lavender })));
        if !tail.is_empty() {
            spans.push(Span::styled(tail, bar.fg(p.peach)));
        }
    }
    spans.push(Span::styled(" ".repeat(pad), bar));
    // The suffix repaints in parts so the totals get the file rows' green/red; the parts spell
    // out `header_suffix`, which the alignment math measures.
    let (added, removed) = app.changed_totals();
    spans.push(Span::styled(format!("{} changed", app.changed_count()), bar.fg(p.overlay0)));
    let stats = stats_spans(added, removed, p);
    if !stats.is_empty() {
        spans.push(Span::styled("  ", bar));
        spans.extend(stats.into_iter().map(|s| Span::styled(s.content, s.style.bg(p.surface0))));
    }
    spans.push(Span::styled(HEADER_LEAD, bar));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_file_list(frame: &mut Frame, app: &App, area: Rect) {
    let p = app.palette();
    let block = bordered("Files", app.focus == Focus::Files, p);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.file_rows.is_empty() {
        let msg = if app.awaiting_turn() { app.turn_wait_message() } else { "no changes" };
        frame.render_widget(dim_paragraph(msg, p), inner);
        return;
    }

    let width = inner.width as usize;
    // Window the rows to the scrolled-to viewport; `file_scroll` keeps the cursor on screen.
    let items: Vec<ListItem> = app
        .file_rows
        .iter()
        .enumerate()
        .skip(app.file_scroll)
        .take(inner.height as usize)
        .map(|(i, row)| {
            // The selected row fills with the cursor color, dimmed when the list is unfocused.
            let fill = (i == app.file_cursor).then(|| p.cursor_bg(app.focus == Focus::Files));
            let indent = "  ".repeat(row.depth);
            match &row.kind {
                RowKind::Dir { expanded, .. } => {
                    let arrow = if *expanded { "▾ " } else { "▸ " };
                    let name_style = Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD);
                    let spans = vec![
                        Span::styled(format!("{indent}{arrow}"), Style::default().fg(p.overlay0)),
                        Span::styled(format!("{}/", row.name), name_style),
                    ];
                    selectable_row(p, spans, width, fill)
                }
                RowKind::File { annotation, .. } => {
                    file_row_item(&indent, annotation, &row.name, width, fill, p)
                }
            }
        })
        .collect();
    frame.render_widget(List::new(items), inner);
}

/// A file row: `<indent><marker> <name> <stats>` — the marker colored by kind, the basename
/// bright with its parent directories dimmed, and the `+a −d` stats right-aligned against the
/// pane edge. A name too wide for the row keeps its tail behind a leading `…/`.
fn file_row_item(
    indent: &str,
    annotation: &Annotation,
    name: &str,
    width: usize,
    fill: Option<Color>,
    p: &Palette,
) -> ListItem<'static> {
    let marker = format!("{} ", annotation.change.marker());
    let (additions, deletions) = (annotation.additions, annotation.deletions);
    let stats = stats_str(additions, deletions);
    let gap = if stats.is_empty() { 0 } else { 2 };
    let fixed = indent.width() + marker.width() + stats.width() + gap;
    let shown = elide_head(name, width.saturating_sub(fixed).max(1));

    let marker_style = Style::default().fg(kind_color(p, annotation.change.marker()));
    let mut spans =
        vec![Span::styled(indent.to_string(), text_style(p)), Span::styled(marker, marker_style)];
    // Dim the parent directories of a collapsed-chain name, keep the basename bright.
    let (dim, base) = match shown.rfind('/') {
        Some(s) => (&shown[..=s], &shown[s + 1..]),
        None => ("", shown.as_str()),
    };
    if !dim.is_empty() {
        spans.push(Span::styled(dim.to_string(), Style::default().fg(p.overlay0)));
    }
    spans.push(Span::styled(base.to_string(), text_style(p)));
    if !stats.is_empty() {
        let used: usize = spans.iter().map(Span::width).sum();
        let pad = width.saturating_sub(used + stats.width());
        spans.push(Span::raw(" ".repeat(pad)));
        spans.extend(stats_spans(additions, deletions, p));
    }
    selectable_row(p, spans, width, fill)
}

/// The `+a −d` stats text, dropping a side that is zero (`+210`, `−4`, or empty); used to
/// measure the stats column. [`stats_spans`] paints the same text in green/red.
fn stats_str(additions: u32, deletions: u32) -> String {
    match (additions, deletions) {
        (0, 0) => String::new(),
        (a, 0) => format!("+{a}"),
        (0, d) => format!("−{d}"),
        (a, d) => format!("+{a} −{d}"),
    }
}

/// The `+a −d` stats as colored spans: additions in green, deletions in red, matching the
/// diff's add/remove hues. Same glyphs (and width) as [`stats_str`].
fn stats_spans(additions: u32, deletions: u32, p: &Palette) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if additions > 0 {
        spans.push(Span::styled(format!("+{additions}"), Style::default().fg(p.green)));
    }
    if additions > 0 && deletions > 0 {
        spans.push(Span::raw(" "));
    }
    if deletions > 0 {
        spans.push(Span::styled(format!("−{deletions}"), Style::default().fg(p.red)));
    }
    spans
}

/// Shorten `name` to `max` columns by eliding its head behind a leading `…`, preferring to
/// cut at a path separator so a partial directory name never shows.
fn elide_head(name: &str, max: usize) -> String {
    if name.width() <= max {
        return name.to_string();
    }
    let budget = max.saturating_sub(1); // a column for the `…`
    let mut tail = String::new();
    let mut w = 0;
    for ch in name.chars().rev() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > budget {
            break;
        }
        tail.insert(0, ch);
        w += cw;
    }
    if let Some(slash) = tail.find('/') {
        tail = tail[slash..].to_string();
    }
    format!("…{tail}")
}

/// A saved comment as inline display lines: a quiet box titled with the comment's location
/// (in the comment-yellow accent) holding its wrapped text. Spliced read-only under the
/// commented line so a submitted comment stays visible while reviewing.
fn comment_card_lines(c: &Comment, width: usize, p: &Palette) -> Vec<Line<'static>> {
    const INDENT: usize = 2;
    let box_w = width.saturating_sub(INDENT).max(10);
    let text_w = box_w.saturating_sub(4).max(1); // inside "│ " … " │"
    let border = Style::default().fg(p.overlay0);
    let title = Style::default().fg(p.peach).add_modifier(Modifier::BOLD);
    let body_style = Style::default().fg(p.text);
    let pad = || Span::raw(" ".repeat(INDENT));

    let label = truncate_width(&format!(" comment · {} ", c.location()), box_w.saturating_sub(3));
    let fill = box_w.saturating_sub(3 + label.width());
    let mut lines = vec![Line::from(vec![
        pad(),
        Span::styled("╭─", border),
        Span::styled(label, title),
        Span::styled(format!("{}╮", "─".repeat(fill)), border),
    ])];

    for logical in c.text.split('\n') {
        for piece in wrap_text(logical, text_w) {
            let gap = " ".repeat(text_w.saturating_sub(piece.width()));
            lines.push(Line::from(vec![
                pad(),
                Span::styled("│ ", border),
                Span::styled(piece, body_style),
                Span::styled(format!("{gap} │"), border),
            ]));
        }
    }

    lines.push(Line::from(vec![
        pad(),
        Span::styled(format!("╰{}╯", "─".repeat(box_w.saturating_sub(2))), border),
    ]));
    lines
}

/// Truncate `s` to `max` display columns, marking a cut with a trailing `…`. Zero columns
/// fit nothing, not a bare `…`.
fn truncate_width(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

fn render_diff_view(frame: &mut Frame, app: &App, area: Rect) {
    let p = app.palette();
    let mut title = match (&app.diff_path, &app.diff.previous_path) {
        (Some(new), Some(old)) => format!("{old} → {new}"),
        (Some(new), None) => new.clone(),
        (None, _) => "Diff".to_string(),
    };
    if app.preview_active() {
        title.push_str(" · preview");
    }
    let block = bordered(&title, app.focus == Focus::Diff, p);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.note_diff_width(inner.width as usize);

    if app.visible.is_empty() {
        let msg = match app.tab {
            Tab::Changes if app.awaiting_turn() => app.turn_wait_message(),
            _ => match app.diff.state {
                FileState::Binary => "binary — no line comments",
                FileState::TooLarge => "file too large to diff",
                FileState::Normal => "no diff",
            },
        };
        frame.render_widget(dim_paragraph(msg, p), inner);
        return;
    }

    let height = inner.height as usize;
    if height == 0 {
        return;
    }
    let width = inner.width as usize;

    // The markdown preview: rendered lines, no gutter, no cursor; the scroll clamps to
    // the rendered length so a refresh that shrank the file keeps the reader in range
    // (specs/diff-view.md).
    if app.preview_active() {
        let rendered = app.markdown_render(app.preview_text(), width.max(1));
        // Scrolling stops with the last line at the pane's bottom edge; content that
        // fits the pane does not scroll.
        let max = rendered.lines.len().saturating_sub(height);
        app.note_preview_max_scroll(max);
        let scroll = app.preview_scroll.min(max);
        note_markdown_regions(app, &rendered, inner, scroll, 0);
        frame.render_widget(
            Paragraph::new(rendered.lines).scroll((saturating_row(scroll), 0)),
            inner,
        );
        render_overflow_scrollbar(
            frame,
            area.inner(ratatui::layout::Margin { vertical: 1, horizontal: 0 }),
            max,
            scroll,
            p,
        );
        return;
    }

    let gutter_w = gutter_for(&app.diff);
    let layout = RowLayout {
        gutter_w,
        width,
        h_scroll: app.h_scroll,
        wrap: app.wrap,
        focused: app.focus == Focus::Diff,
        pal: p,
        find: app
            .find
            .as_ref()
            .map(|f| (f.query.as_str(), crate::app::find_case_sensitive(&f.query))),
    };
    let commented = app.commented_lines();
    let cards = app.comment_cards();
    let editing = editing_comment(app);
    let (lo, hi) = app.selection_range();
    let selecting = app.focus == Focus::Diff && app.select_anchor.is_some();

    // One logical row → its 1+ wrapped display lines, then any saved-comment cards anchored
    // to it. The cursor/selection apply to the code line's display rows, not the cards. The
    // card of a comment being edited is hidden — its edit box stands in for it.
    let row_lines = |i: usize| -> Vec<Line> {
        let state = RowState {
            // The cursor row is always marked, dimmed while the pane is unfocused, exactly as
            // the file list marks its own (`specs/input.md`). A hunk step driven from the list
            // moves this cursor, so hiding it would leave the jump with nothing to show.
            commented: commented.contains(&i),
            cursor: i == app.diff_cursor,
            selected: selecting && i >= lo && i <= hi,
        };
        let mut lines = render_row(&app.visible[i], layout, state);
        for &ci in &cards[i] {
            if Some(ci) != editing
                && let Some(c) = app.store.get(ci)
            {
                lines.extend(comment_card_lines(c, width, p));
            }
        }
        lines
    };
    // Display lines for the logical rows in `range`, in order.
    let display = |range: std::ops::Range<usize>| -> Vec<Line> {
        range.flat_map(&row_lines).collect::<Vec<_>>()
    };

    let rows = app.visible.len();
    if !app.composing() {
        // The find band takes the pane's bottom row while it is open (specs/find-in-file.md).
        let finding = app.mode == Mode::Find;
        let body_h = if finding { height.saturating_sub(1) } else { height };
        // Fill the pane from `diff_scroll`'s first display line; clamp keeps the cursor in.
        let mut out = display(app.diff_scroll..rows);
        out.truncate(body_h);
        frame.render_widget(Paragraph::new(out), Rect { height: body_h as u16, ..inner });
        if finding {
            let band = Rect { y: inner.y + body_h as u16, height: 1, ..inner };
            render_find_band(frame, app, band);
        }
        return;
    }

    // Composing: splice the input box under the last selected line, in display rows.
    // Cap the box at height-1 so a comment taller than the viewport can't hide its anchor.
    let box_h = composer_height(app, width).min(height.saturating_sub(1)).max(1);
    let diff_budget = height - box_h;
    let anchor = hi.clamp(app.diff_scroll, rows.saturating_sub(1));
    let above = display(app.diff_scroll..anchor + 1);
    // Keep the anchor's last display line just above the box when `above` overflows.
    let above: Vec<Line> =
        if above.len() > diff_budget { above[above.len() - diff_budget..].to_vec() } else { above };
    let remaining = diff_budget - above.len();
    let mut below = display(anchor + 1..rows);
    below.truncate(remaining);

    let slots = Layout::vertical([
        Constraint::Length(above.len() as u16),
        Constraint::Length(box_h as u16),
        Constraint::Length(below.len() as u16),
    ])
    .split(inner);
    if !above.is_empty() {
        frame.render_widget(Paragraph::new(above), slots[0]);
    }
    render_composer(frame, app, slots[1]);
    if !below.is_empty() {
        frame.render_widget(Paragraph::new(below), slots[2]);
    }
}

/// The line-number column width for a diff of `rows` lines.
fn gutter_width(rows: usize) -> usize {
    rows.to_string().len().max(3)
}

/// The gutter width for a whole `FileDiff`, sized to its largest line number so it does not
/// resize when a fold toggles (folds hide lines but keep the numbering). One definition,
/// shared by `diff_row_heights` (measuring) and `render_diff_view` (painting), so the
/// measured and painted geometry can never disagree.
fn gutter_for(diff: &FileDiff) -> usize {
    let total_lines: usize =
        diff.rows.iter().map(|r| if r.is_content() { 1 } else { r.hidden() }).sum();
    gutter_width(total_lines)
}

/// The gutter prefix width: the change bar plus the right-aligned line number and a space.
fn gutter_prefix_width(gutter_w: usize) -> usize {
    1 + gutter_w + 1
}

/// How many display rows a row needs: 1 for a fold or with wrap off, else the number of
/// word-wrapped segments its (tab-expanded) content fills. Shares [`wrap_segments`] with
/// the renderer so per-row geometry stays aligned with what gets painted.
fn row_height(row: &Row, gutter_w: usize, width: usize, wrap: bool) -> usize {
    if !wrap || matches!(row, Row::Fold { .. }) {
        return 1;
    }
    let code_width = width.saturating_sub(gutter_prefix_width(gutter_w)).max(1);
    // The find highlight never changes wrapping, so height ignores it.
    wrap_segments(&code_cells(row, false, &[]), code_width, ContinuationSpaces::Trim).len()
}

/// The diff-pane layout: constant for a frame.
#[derive(Clone, Copy)]
struct RowLayout<'a> {
    gutter_w: usize,
    width: usize,
    h_scroll: usize,
    wrap: bool,
    /// Whether the diff pane is focused — dims the cursor row when it is not.
    focused: bool,
    /// The active palette for the change bars, row tints, and fills.
    pal: &'a Palette,
    /// The in-file find query and its smart-case flag while the band is open, so every visible
    /// row lights its matches (specs/find-in-file.md).
    find: Option<(&'a str, bool)>,
}

/// A row's per-row highlight state.
#[derive(Clone, Copy)]
struct RowState {
    commented: bool,
    cursor: bool,
    selected: bool,
}

/// A diff row as one or more full-width display lines: a left change bar, the line
/// number, then syntax-colored code tinted red/green. With wrap on, a long line breaks
/// into `code_width`-wide rows; a continuation row carries a blank gutter so numbers
/// stay aligned. With wrap off, the line is one row scrolled by `h_scroll`.
fn render_row(row: &Row, layout: RowLayout<'_>, state: RowState) -> Vec<Line<'static>> {
    let RowLayout { gutter_w, width, h_scroll, wrap, focused, pal, find } = layout;
    let RowState { commented, cursor, selected } = state;
    if let Row::Fold { .. } = row {
        let label = if cursor {
            format!("  ⋯  {} unmodified lines — → expand", row.hidden())
        } else {
            format!("  ⋯  {} unmodified lines", row.hidden())
        };
        let mut line = Line::from(Span::styled(label, Style::default().fg(pal.subtext0)));
        if let Some(pad) = width.checked_sub(line.width()).filter(|p| *p > 0) {
            line.push_span(Span::raw(" ".repeat(pad)));
        }
        let bg = if cursor { pal.cursor_bg(focused) } else { pal.surface0 };
        return vec![line.style(Style::default().bg(bg).add_modifier(Modifier::BOLD))];
    }
    let num = row.new_no().or_else(|| row.old_no()).map_or(String::new(), |n| n.to_string());
    // A commented line's number takes the peach comment accent; others sit a step brighter
    // than the dim chrome so they stay legible while read.
    let num_color = if commented { pal.peach } else { pal.overlay1 };
    let (bar, bar_color) = match row.marker() {
        '-' => ("▌", pal.red),
        '+' => ("▌", pal.green),
        _ => (" ", pal.overlay0),
    };
    let row_bg = if cursor {
        Some(pal.cursor_bg(focused))
    } else if selected {
        Some(pal.surface1)
    } else {
        match row.marker() {
            '-' => Some(pal.del_bg),
            '+' => Some(pal.ins_bg),
            _ => None,
        }
    };

    // Word emphasis brightens the changed words, unless the row's fill is a cursor or
    // selection bg, which wins for readability.
    let emph_on = !cursor && !selected;
    let emph_bg = match row.marker() {
        '-' => pal.emph_del_bg,
        '+' => pal.emph_ins_bg,
        _ => pal.ins_bg,
    };
    // The find highlight lays `match_hl` behind the query's matches on this row, char-indexed
    // like word emphasis (specs/find-in-file.md).
    let hl_ranges =
        find.map(|(q, cs)| crate::app::find_match_ranges(&row.text(), q, cs)).unwrap_or_default();
    let cells = code_cells(row, emph_on, &hl_ranges);

    let prefix_w = gutter_prefix_width(gutter_w);
    let code_width = width.saturating_sub(prefix_w).max(1);
    // Without wrap the line is one chunk scrolled by `h_scroll`; with wrap, word-wrapped
    // segments, the first numbered and the rest blank-gutter.
    let chunks: Vec<&[Cell]> = if wrap {
        wrap_segments(&cells, code_width, ContinuationSpaces::Trim)
            .into_iter()
            .map(|(s, e)| &cells[s..e])
            .collect()
    } else {
        vec![cells.get(skip_columns(&cells, h_scroll)..).unwrap_or(&[])]
    };

    chunks
        .into_iter()
        .enumerate()
        .map(|(k, chunk)| {
            let gutter = if k == 0 {
                vec![
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(format!("{num:>gutter_w$} "), Style::default().fg(num_color)),
                ]
            } else {
                // A continuation row keeps the change bar but blanks the number column.
                vec![
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::raw(" ".repeat(prefix_w - 1)),
                ]
            };
            let mut spans = gutter;
            spans.extend(cells_to_spans(
                chunk,
                emph_bg,
                HlStyle { bg: pal.yellow, fg: pal.surface0 },
            ));
            let mut line = Line::from(spans);
            if let Some(pad) = width.checked_sub(line.width()).filter(|p| *p > 0) {
                line.push_span(Span::raw(" ".repeat(pad)));
            }
            match row_bg {
                Some(bg) => line.style(Style::default().bg(bg)),
                None => line,
            }
        })
        .collect()
}

pub(crate) fn rgb(c: crate::diff::Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// Tabs expand to this many columns.
const TAB: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContinuationSpaces {
    Keep,
    Trim,
}

fn plain_cell(ch: char) -> Cell {
    Cell {
        ch,
        w: UnicodeWidthChar::width(ch).unwrap_or(0),
        fg: Color::Reset,
        emph: false,
        hl: false,
    }
}

/// Greedy word wrap over display cells into half-open ranges, one per display row.
///
/// Breaks at the last space that fits within `width`, falling back to a hard break when a
/// single word is wider than the column. [`ContinuationSpaces::Trim`] drops leading spaces
/// from continuation rows; [`ContinuationSpaces::Keep`] preserves every character for caret
/// mapping. An empty line still yields one range. The renderer and [`row_height`] share this
/// so what's measured matches what's painted.
fn wrap_segments(
    cells: &[Cell],
    width: usize,
    continuation_spaces: ContinuationSpaces,
) -> Vec<(usize, usize)> {
    if cells.is_empty() {
        return vec![(0, 0)];
    }
    let mut segs = Vec::new();
    let mut start = 0;
    while start < cells.len() {
        // Take as many cells as fit within `width` columns, always at least one (so a glyph
        // wider than the column still gets its own row rather than stalling).
        let mut col = 0;
        let mut limit = start;
        while limit < cells.len() {
            let cw = cells[limit].w;
            if col + cw > width && limit > start {
                break;
            }
            col += cw;
            limit += 1;
        }
        if limit == cells.len() {
            segs.push((start, cells.len()));
            break;
        }
        // More cells follow; prefer breaking just after the last space that fits.
        let brk = (start..limit).rev().find(|&i| cells[i].ch == ' ').map(|i| i + 1);
        let end = brk.filter(|&e| e > start).unwrap_or(limit);
        segs.push((start, end));
        start = end;
        if continuation_spaces == ContinuationSpaces::Trim {
            while start < cells.len() && cells[start].ch == ' ' {
                start += 1;
            }
        }
    }
    segs
}

/// The first cell index lying at or past `cols` display columns — the no-wrap horizontal
/// scroll offset, snapping past a wide glyph that straddles the boundary rather than
/// splitting it.
fn skip_columns(cells: &[Cell], cols: usize) -> usize {
    let mut col = 0;
    let mut i = 0;
    while i < cells.len() && col < cols {
        col += cells[i].w;
        i += 1;
    }
    i
}

/// One display cell of a code line: a glyph, its terminal width in columns (1 for most
/// text, 2 for wide CJK/emoji, 0 for a combining mark), its syntax color, whether it falls in
/// a word-emphasis range, and whether it falls in an in-file find match (specs/find-in-file.md).
struct Cell {
    ch: char,
    w: usize,
    fg: Color,
    emph: bool,
    hl: bool,
}

/// Expand a row's spans into display cells: tabs become spaces to the next tab stop, and each
/// char carries its column width, color, its word-emphasis flag (when `emph_on`), and whether it
/// falls in an in-file find match (`hl_ranges`, char indices). Width comes from `unicode-width`
/// so wide glyphs measure as the two columns they paint.
fn code_cells(row: &Row, emph_on: bool, hl_ranges: &[(u32, u32)]) -> Vec<Cell> {
    let emphasis = if emph_on { row.emphasis() } else { &[] };
    let in_emph = |i: u32| emphasis.iter().any(|&(a, b)| i >= a && i < b);
    let in_hl = |i: u32| hl_ranges.iter().any(|&(a, b)| i >= a && i < b);
    let mut cells = Vec::new();
    let mut idx = 0u32;
    let mut col = 0usize; // display column, so tab stops land right after wide glyphs too
    for s in row.spans() {
        let fg = rgb(s.color);
        for ch in s.text.chars() {
            let emph = in_emph(idx);
            let hl = in_hl(idx);
            if ch == '\t' {
                for _ in 0..(TAB - col % TAB) {
                    cells.push(Cell { ch: ' ', w: 1, fg, emph, hl });
                    col += 1;
                }
            } else {
                let w = UnicodeWidthChar::width(ch).unwrap_or(0);
                cells.push(Cell { ch, w, fg, emph, hl });
                col += w;
            }
            idx += 1;
        }
    }
    cells
}

/// Build spans from display cells, merging runs of equal color, emphasis, and find-highlight; a
/// highlighted run takes `hl_bg` (the find match), else an emphasized run takes `emph_bg`.
fn cells_to_spans(cells: &[Cell], emph_bg: Color, hl: HlStyle) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut cur: Option<(Color, bool, bool)> = None;
    for c in cells {
        let key = (c.fg, c.emph, c.hl);
        if cur != Some(key) {
            if let Some((fg, emph, is_hl)) = cur {
                spans.push(cell_span(std::mem::take(&mut buf), fg, emph, is_hl, emph_bg, hl));
            }
            cur = Some(key);
        }
        buf.push(c.ch);
    }
    if let Some((fg, emph, is_hl)) = cur {
        spans.push(cell_span(buf, fg, emph, is_hl, emph_bg, hl));
    }
    spans
}

/// The find match's reverse-highlight colors: a bright fill and the dark text drawn on it, so a
/// match reads over any row tint, red or green (specs/find-in-file.md).
#[derive(Clone, Copy)]
struct HlStyle {
    bg: Color,
    fg: Color,
}

/// A run's span: a find match reverses to `hl.fg` on `hl.bg`; else word emphasis takes `emph_bg`;
/// else the plain foreground (specs/find-in-file.md).
fn cell_span(
    text: String,
    fg: Color,
    emph: bool,
    is_hl: bool,
    emph_bg: Color,
    hl: HlStyle,
) -> Span<'static> {
    let style = if is_hl {
        Style::default().fg(hl.fg).bg(hl.bg).add_modifier(Modifier::BOLD)
    } else if emph {
        Style::default().fg(fg).bg(emph_bg)
    } else {
        Style::default().fg(fg)
    };
    Span::styled(text, style)
}

/// The find band at the read pane's foot: the `find` label, the query with its block caret, and
/// the match count at the right. The single-line query scrolls horizontally to keep the caret in
/// view (specs/find-in-file.md).
fn render_find_band(frame: &mut Frame, app: &App, area: Rect) {
    let Some(f) = app.find.as_ref() else { return };
    let p = app.palette();
    let dim = Style::default().fg(p.overlay0);

    // The count: `k/total` on a match, the total off a match, `no matches` when nothing matches,
    // blank while the query is empty (specs/find-in-file.md).
    let count = match app.find_count() {
        None => String::new(),
        Some((_, 0)) => "no matches".to_string(),
        Some((Some(k), total)) => format!("{k}/{total}"),
        Some((None, total)) => total.to_string(),
    };

    let width = area.width as usize;
    let count_w = count.width();
    let label = "find ";
    let query_w = width.saturating_sub(label.width() + count_w + 1).max(1);

    let mut spans = vec![Span::styled(label, Style::default().fg(p.subtext0))];
    if f.query.is_empty() {
        spans.extend(row_with_caret("", 0, p).spans);
        spans.push(Span::styled("find in file…", dim));
    } else {
        // Scroll the query so the caret stays visible on a query longer than the band, and bound
        // the slice to `query_w` so a long tail never pushes the count off the right edge.
        let caret_col = f.caret.min(f.query.chars().count());
        let chars: Vec<char> = f.query.chars().collect();
        let start = caret_col.saturating_sub(query_w.saturating_sub(1));
        let visible: String = chars[start.min(chars.len())..].iter().take(query_w).collect();
        spans.extend(row_with_caret(&visible, caret_col - start, p).spans);
    }

    let mut line = Line::from(spans);
    if let Some(pad) = width.checked_sub(line.width() + count_w).filter(|pad| *pad > 0) {
        line.push_span(Span::raw(" ".repeat(pad)));
    }
    if !count.is_empty() {
        line.push_span(Span::styled(count, dim));
    }
    frame.render_widget(Paragraph::new(line), area);
}

/// The inline comment input box, drawn at `area` (under the selection in the diff).
fn render_composer(frame: &mut Frame, app: &App, area: Rect) {
    let p = app.palette();
    let loc = app.pending_location().unwrap_or_else(|| "comment".to_string());
    let editing = matches!(app.mode, Mode::Composing { editing: Some(_) });
    let title = if editing { format!("edit · {loc}") } else { format!("comment · {loc}") };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.peach))
        .title(framed_title(&title));
    let content_w = composer_content_width(area.width as usize);
    let body = Paragraph::new(composer_lines(app, content_w)).block(block);
    frame.render_widget(body, area);
}

/// The key glyph and label for a footer action; an empty label renders the glyph alone. The
/// `TogglePane` and `Send` labels depend on `app` (the destination pane, the comment count).
fn action_key_label(app: &App, action: FooterAction) -> (String, String) {
    use crate::keymap::Action as K;
    use FooterAction as A;
    // A rebindable action's hint is its first bound key (`specs/input.md`).
    let hint = |action: K| app.keymap().hint(action).to_string();
    let (k, l): (String, &str) = match action {
        A::Comment => (hint(K::Comment), "comment"),
        A::Select => (hint(K::Select), "select"),
        A::ClearSelection => ("esc".into(), "clear"),
        A::EditComment => (hint(K::Edit), "edit"),
        A::DeleteComment => (hint(K::Delete), "delete"),
        A::JumpComment => (format!("{}/{}", hint(K::NextComment), hint(K::PrevComment)), "jump"),
        A::ExpandFold => ("→".into(), "expand fold"),
        // The armed crossing is keyed to the hunk step that armed it, so a rebound `next-hunk`
        // is the key the hint shows.
        A::CrossFile { forward: true } => (hint(K::NextHunk), "next file"),
        A::CrossFile { forward: false } => (hint(K::PrevHunk), "prev file"),
        // The `move` band's pairs render as their two keys; `MovePage`'s are the fixed page keys.
        A::MoveLine => (format!("{} {}", hint(K::Down), hint(K::Up)), ""),
        A::MoveHunk => (format!("{} {}", hint(K::NextHunk), hint(K::PrevHunk)), "hunk"),
        A::MoveFile => (format!("{} {}", hint(K::NextFile), hint(K::PrevFile)), "file"),
        A::MovePage => ("PageUp PageDown".into(), ""),
        A::ExpandDir => ("→".into(), "expand"),
        A::CollapseDir => ("←".into(), "collapse"),
        A::TogglePane => {
            return ("tab".into(), if app.focus == Focus::Files { "diff" } else { "files" }.into());
        }
        A::Preview => (hint(K::Preview), if app.preview_active() { "source" } else { "preview" }),
        A::NavigatorPosition => (hint(K::NavigatorPosition), "position"),
        A::NavigatorHide => {
            (hint(K::NavigatorHide), if app.navigator_hidden_here() { "show" } else { "hide" })
        }
        A::Scope => (
            format!(
                "{}/{}/{}",
                hint(K::ScopeUncommitted),
                hint(K::ScopeBranch),
                hint(K::ScopeLastTurn)
            ),
            "scope",
        ),
        A::Send => return (hint(K::Send), format!("send {}", app.store.len())),
        A::List => (hint(K::Comments), "list"),
        A::Copy => (hint(K::Copy), "copy"),
        A::Save => ("enter".into(), "save"),
        A::Newline => ("shift+enter".into(), "newline"),
        A::Cancel | A::ClosePicker => ("esc".into(), "cancel"),
        A::CloseList | A::CloseFind => ("esc".into(), "close"),
        A::PickAgent => ("enter".into(), "send"),
        // The digits are literal, so they are spelled; the two movement keys are bound, so they
        // read off the keymap like every other hint (`specs/input.md`).
        A::MovePickerRow => (format!("1-9 {} {}", hint(K::Down), hint(K::Up)), "move"),
        A::BasePick => (hint(K::BasePick), "pick base"),
        A::PickBaseRow => ("enter".into(), "pick"),
        // Every printable is filter text in the base picker, so only the arrows move
        // (`specs/input.md` Base picker).
        A::MoveBaseRow => ("↑↓".into(), "move"),
        A::ScopeOther => {
            use crate::model::Scope;
            let others: Vec<String> = [
                (Scope::Uncommitted, K::ScopeUncommitted),
                (Scope::Branch, K::ScopeBranch),
                (Scope::LastTurn, K::ScopeLastTurn),
            ]
            .into_iter()
            .filter(|(scope, _)| app.scope != *scope)
            .map(|(_, action)| hint(action))
            .collect();
            (others.join("/"), "scope")
        }
        A::Find => (hint(K::Find), "find"),
        A::Wrap => (hint(K::Wrap), "wrap"),
        A::FindStep => ("↑↓".into(), "match"),
        A::OpenPr => (hint(K::OpenPr), "open ↗"),
        A::Refresh => (hint(K::Refresh), "refresh"),
        A::Tabs => {
            (format!("{}·{}·{}", hint(K::TabChanges), hint(K::TabPr), hint(K::TabIssues)), "tabs")
        }
        A::Quit => (hint(K::Quit), "quit"),
    };
    (k, l.into())
}

/// A band's `(key, label)` styles: the primary bright and bold, every other action readable. The
/// `?` expansion's own dim band labels (`do`/`go`/`move`) are styled separately (`render_band`).
fn band_styles(band: Band, p: &Palette) -> (Style, Style) {
    match band {
        Band::Primary => (Style::default().fg(p.peach).add_modifier(Modifier::BOLD), text_style(p)),
        Band::Send | Band::Do | Band::Go | Band::Move => {
            (Style::default().fg(p.lavender), Style::default().fg(p.subtext0))
        }
    }
}

/// The ` · `-separated `key label` spans for one action, styled by its band. The leading separator
/// is the caller's to add, so a wrapped band row can start without one.
fn action_entry(app: &App, action: FooterAction, band: Band) -> Vec<Span<'static>> {
    let p = app.palette();
    let (key, label) = action_key_label(app, action);
    let (key_style, label_style) = band_styles(band, p);
    let mut spans = vec![Span::styled(key, key_style)];
    if !label.is_empty() {
        spans.push(Span::styled(format!(" {label}"), label_style));
    }
    spans
}

/// The rendered width of one action entry: its `key label` (a space joins them).
fn entry_body_width(app: &App, action: FooterAction) -> usize {
    let (key, label) = action_key_label(app, action);
    if label.is_empty() {
        key.chars().count()
    } else {
        key.chars().count() + 1 + label.chars().count()
    }
}

/// The rendered width of one action entry, plus its leading ` · ` separator.
fn entry_width(app: &App, action: FooterAction) -> usize {
    SEP.chars().count() + entry_body_width(app, action)
}

/// The ` · ` that joins footer entries, and the dim-label indent of a wrapped `?`-band row.
const SEP: &str = " · ";
const BAND_INDENT: usize = 6;

/// The footer status's frame: the two-space gap, the `·`, and the trailing space around it.
const STATUS_FRAME: usize = 5;
/// The narrowest status row 1 paints. Below it a lone `·` would promise a message the row has no
/// room to show, so the status drops instead (`specs/input.md`).
const STATUS_MIN: usize = 8;
/// The ` …` a modal footer ends with when an action was trimmed off row 1. It stands in for the
/// `?` a modal does not have, so it is the only promise that more keys exist.
const MORE_ELLIPSIS: usize = 2;

/// The footer: row 1 (the primary, the cursor's actions, `send`, and a `?`), plus the wrapped
/// `?`-expansion bands below when it is open. Row 1 trims trailing actions to fit; the primary,
/// `send`, and `?` never drop, and the bands are capped so the body keeps its rows (`specs/input.md`).
fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let p = app.palette();
    let mut lines = footer_lines(app, area.width as usize);
    lines.truncate((area.height as usize).max(1));
    frame.render_widget(Paragraph::new(lines).style(Style::default().bg(p.surface0)), area);
}

/// The footer's height for the vertical layout: one row collapsed, one plus the wrapped bands when
/// the `?` expansion is open, capped so the body keeps its `Min(3)` (`specs/input.md`, `tui.md`).
fn footer_height(app: &App, area: Rect) -> u16 {
    if !(app.keys_expanded && app.mode == Mode::Normal) {
        return 1;
    }
    let want = footer_lines(app, area.width as usize).len() as u16;
    let cap = area.height.saturating_sub(1 + 3).max(1); // tab bar + body minimum
    want.clamp(1, cap)
}

/// Row 1 followed by the expansion's labeled bands (when open). One builder, so the layout's height
/// and the paint agree by construction.
fn footer_lines(app: &App, w: usize) -> Vec<Line<'static>> {
    let (row1, overflow) = footer_row1(app, w);
    let mut lines = vec![Line::from(row1)];
    if app.keys_expanded && app.mode == Mode::Normal {
        let bands = app.footer_bands();
        let of_band = |band: Band| -> Vec<FooterAction> {
            bands.iter().filter(|&&(_, b)| b == band).map(|&(a, _)| a).collect()
        };
        // Row 1 already carries the `do` label, so its overflow continues under a blank gutter,
        // aligned with row 1's content; an empty overflow is dropped.
        lines.extend(render_band(app, w, "", Band::Do, &overflow));
        lines.extend(render_band(app, w, "go", Band::Go, &of_band(Band::Go)));
        lines.extend(render_band(app, w, "move", Band::Move, &of_band(Band::Move)));
    }
    lines
}

/// Row 1: the primary, the cursor's `Do` actions (trimmed to fit), `send`, the transient status,
/// and a right-aligned `?` in `Normal` mode. Returns the trimmed-off `Do` actions for the `do` band.
fn footer_row1(app: &App, w: usize) -> (Vec<Span<'static>>, Vec<FooterAction>) {
    let p = app.palette();
    let bands = app.footer_bands();
    let primary = bands.iter().find(|&&(_, b)| b == Band::Primary).map(|&(a, _)| a);
    let do_acts: Vec<FooterAction> =
        bands.iter().filter(|&&(_, b)| b == Band::Do).map(|&(a, _)| a).collect();
    let send = bands.iter().find(|&&(_, b)| b == Band::Send).map(|&(a, _)| a);
    let show_more = app.mode == Mode::Normal;
    let reserve = if show_more { 2 } else { 0 }; // a gap plus the `?`

    // `send` and the `?` share the right of the row and never drop, so the primary and the actions
    // both yield to keep them on the line — the primary reserves their width before anything else.
    let send_w = send.map_or(0, |a| entry_width(app, a));
    let tail = send_w + reserve;

    // While the panel is open, row 1 joins the labeled grid: a dim `do` gutter, its content aligned
    // under the `go`/`move` keys. Collapsed (and in a modal) it stays flush — the plain action bar.
    // The grid engages only when the gutter still leaves room for the primary's key, `send`, and the
    // `?`; below that the flush row keeps them, since the fixed gutter cannot shed the way the row's
    // own content does.
    let primary_key_w = primary.map_or(0, |a| action_key_label(app, a).0.chars().count());
    let labeled = app.keys_expanded
        && show_more
        && (primary.is_some() || !do_acts.is_empty())
        && 1 + BAND_INDENT + primary_key_w + tail <= w;
    let (mut spans, mut used): (Vec<Span<'static>>, usize) = if labeled {
        let label =
            Span::styled(format!("{:<BAND_INDENT$}", "do"), Style::default().fg(p.overlay0));
        (vec![Span::raw(" "), label], 1 + BAND_INDENT)
    } else {
        (vec![Span::raw(" ")], 1)
    };

    // The read-only PR tab leads with the PR's state summary, capped so the primary and the `?`
    // keep their room on the line.
    let pr_state = (app.tab == Tab::Pr).then(|| app.pr_snapshot()).flatten();
    if let Some(s) = pr_state {
        let primary_w = primary.map_or(0, |a| entry_body_width(app, a));
        let budget = w.saturating_sub(used + primary_w + reserve + 4).max(8);
        let text = truncate_width(&format!("{}   ", pr_state_line(app, s)), budget);
        used += text.chars().count();
        spans.push(Span::styled(text, Style::default().fg(p.subtext0)));
    }

    // The primary never drops; on a pane too narrow for it, `send`, and the `?`, it sheds its label,
    // then truncates its key (`specs/input.md`).
    if let Some(a) = primary {
        let (key, label) = action_key_label(app, a);
        let (key_style, label_style) = band_styles(Band::Primary, p);
        let full =
            key.chars().count() + if label.is_empty() { 0 } else { 1 + label.chars().count() };
        if used + full + tail <= w || !show_more {
            spans.push(Span::styled(key, key_style));
            if !label.is_empty() {
                spans.push(Span::styled(format!(" {label}"), label_style));
            }
            used += full;
        } else if used + key.chars().count() + tail <= w {
            used += key.chars().count();
            spans.push(Span::styled(key, key_style));
        } else {
            let room = w.saturating_sub(used + tail);
            let key = truncate_width(&key, room);
            used += key.chars().count();
            spans.push(Span::styled(key, key_style));
        }
    }

    // The status answers the keypress the reviewer just made and fades on its own clock, so it
    // outranks the cursor's actions: the `?` panel repeats every action, and nothing repeats the
    // status (`specs/input.md`). It reserves its room here, before the actions pack into what is
    // left, so a 40-column pane still shows the send's outcome.
    //
    // The reservation is what the status can actually take, never what it wants: capped at the
    // room that exists, and nothing at all when even that is below `STATUS_MIN`. Reserving the
    // untruncated width would evict every action for a message the paint below then drops,
    // leaving a row with neither. The worst-case tail is the same either way, since a trimmed
    // modal footer spends on its `…` exactly what a `Normal` one spends on its `?`.
    let status_tail =
        send_w + if do_acts.is_empty() { reserve } else { reserve.max(MORE_ELLIPSIS) };
    let free = w.saturating_sub(used + status_tail);
    let status_w = if app.status.is_empty() || free < STATUS_FRAME + STATUS_MIN {
        0
    } else {
        (STATUS_FRAME + app.status.width()).min(free)
    };

    // The cursor's actions, packed until one would crowd `send` and the `?` off the line; the rest
    // spill to the `do` band.
    let mut overflow = Vec::new();
    let mut trimming = false;
    for a in do_acts {
        let ew = entry_width(app, a);
        if trimming || used + ew + send_w + status_w + reserve > w {
            trimming = true;
            overflow.push(a);
            continue;
        }
        used += ew;
        spans.push(Span::styled(SEP, Style::default().fg(p.overlay0)));
        spans.extend(action_entry(app, a, Band::Do));
    }

    // `send` closes the actions and never drops.
    if let Some(a) = send {
        used += send_w;
        spans.push(Span::styled(SEP, Style::default().fg(p.overlay0)));
        spans.extend(action_entry(app, a, Band::Send));
    }

    // The transient status rides after the actions, truncated into the room its reservation kept.
    // It drops only below `STATUS_MIN`, where no message would be legible anyway. A modal that
    // trimmed an action keeps room for its `…` too, since nothing else there says more keys exist.
    if !app.status.is_empty() {
        let more = if overflow.is_empty() { reserve } else { reserve.max(MORE_ELLIPSIS) };
        let room = w.saturating_sub(used + STATUS_FRAME + more);
        if room >= STATUS_MIN {
            let text = format!("  · {} ", truncate_width(&app.status, room));
            used += text.width();
            spans.push(Span::styled(text, Style::default().fg(p.peach)));
        }
    }

    // The `?` sits at the right, muted but legible, always present in `Normal` mode. A modal footer
    // has no `?` to promise more, so a trailing `…` marks any action trimmed to fit instead.
    if show_more {
        let pad = w.saturating_sub(used + 1);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled("?", Style::default().fg(p.subtext0)));
    } else if !overflow.is_empty() {
        spans.push(Span::styled(" …", Style::default().fg(p.overlay0)));
    }
    (spans, overflow)
}

/// One `?`-band: a dim label then its keys, wrapped across as many rows as the width needs. The
/// label sits on the first row, continuation rows indent under the keys (`specs/input.md`).
fn render_band(
    app: &App,
    w: usize,
    label: &str,
    band: Band,
    actions: &[FooterAction],
) -> Vec<Line<'static>> {
    if actions.is_empty() {
        return Vec::new();
    }
    let p = app.palette();
    let label_style = Style::default().fg(p.overlay0);
    let avail = w.saturating_sub(1 + BAND_INDENT);
    let start = |first: bool| -> Vec<Span<'static>> {
        if first {
            vec![Span::raw(" "), Span::styled(format!("{label:<BAND_INDENT$}"), label_style)]
        } else {
            vec![Span::raw(" ".repeat(1 + BAND_INDENT))]
        }
    };

    let mut lines = Vec::new();
    let mut row = start(true);
    let mut row_w = 0usize;
    let mut first_in_row = true;
    for &a in actions {
        let entry = action_entry(app, a, band);
        let ew: usize = entry.iter().map(Span::width).sum();
        if !first_in_row && row_w + SEP.chars().count() + ew > avail {
            lines.push(Line::from(std::mem::replace(&mut row, start(false))));
            row_w = 0;
            first_in_row = true;
        }
        if first_in_row {
            row_w += ew;
            first_in_row = false;
        } else {
            row.push(Span::styled(SEP, label_style));
            row_w += SEP.chars().count() + ew;
        }
        row.extend(entry);
    }
    lines.push(Line::from(row));
    lines
}

/// The comments list is a browse surface, not a menu: its box is a fraction of the body rather
/// than its content's size, so the geometry holds still while comments come and go.
const LIST_POPUP_W_PCT: u16 = 80;
const LIST_POPUP_H_PCT: u16 = 70;

fn render_comments_list(frame: &mut Frame, app: &App, area: Rect) {
    let p = app.palette();
    let body = panes(area, app).body;
    let w = body.width * LIST_POPUP_W_PCT / 100;
    let h = body.height * LIST_POPUP_H_PCT / 100;
    let popup = body_popup(area, app, w, h);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.mauve))
        .title(framed_title(&format!("Comments ({})", app.store.len())));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let width = inner.width as usize;
    let items: Vec<ListItem> = app
        .store
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let loc = Span::styled(
                format!(" {}", c.location()),
                Style::default().fg(p.mauve).add_modifier(Modifier::BOLD),
            );
            let mut spans = vec![loc, Span::styled(format!("  {}", c.text), text_style(p))];
            // A comment whose anchor may have moved (file left the changeset, or a content
            // comment's file was deleted) is flagged but kept.
            if app.is_stale(c) {
                spans.push(Span::styled("  (stale)", Style::default().fg(p.red)));
            }
            // The list overlay is the active modal, so its row reads at full brightness.
            selectable_row(p, spans, width, (i == app.list_cursor).then_some(p.surface2))
        })
        .collect();
    frame.render_widget(List::new(items), inner);
}

/// A popup box of `w` × `h`, centered in the body band and clamped to it. Both popups place
/// through here, so neither can ever reach the footer — the one surface advertising the keys
/// the popup is listening for (`specs/tui.md`).
fn body_popup(area: Rect, app: &App, w: u16, h: u16) -> Rect {
    let body = panes(area, app).body;
    let w = w.min(body.width);
    let h = h.min(body.height);
    Rect {
        x: body.x + body.width.saturating_sub(w) / 2,
        y: body.y + body.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

/// The picker is a menu, so its box is sized to its rows rather than to a fraction of the body.
/// Three short rows in an 80%-tall box would be mostly empty (`specs/herdr-host.md`).
const PICKER_MIN_WIDTH: usize = 34;

/// The box any picker menu paints: `widest` row content plus the two borders and one column
/// of air — the air sits inside the row, so the selection fill still reaches both borders.
/// Never narrower than the floor, so a picker of short names still reads as a deliberate
/// dialog.
fn menu_popup(area: Rect, app: &App, widest: usize, title: &str, lines: usize) -> Rect {
    let body = panes(area, app).body;
    let title = framed_title(title).width() + 2;
    let w = (widest + 3).max(title).max(PICKER_MIN_WIDTH).min(body.width as usize) as u16;
    let h = lines.min(body.height as usize) as u16;
    body_popup(area, app, w, h)
}

/// The first visible row, so the highlight stays on screen in a menu taller than the pane
/// (`specs/input.md`).
fn menu_scroll(cursor: usize, total: usize, rows: usize) -> usize {
    if rows == 0 || cursor < rows {
        return 0;
    }
    (cursor + 1).saturating_sub(rows).min(total.saturating_sub(rows))
}

/// The menu row under the pointer, its list starting `top` rows below `inner`'s top and
/// scrolled to `first` — `None` outside the list, so border, title, and filter clicks stay
/// inert (`specs/input.md`).
fn menu_hit(
    inner: Rect,
    top: u16,
    first: usize,
    total: usize,
    col: u16,
    row: u16,
) -> Option<usize> {
    let list_y = inner.y + top;
    if col < inner.x
        || col >= inner.x + inner.width
        || row < list_y
        || row >= inner.y + inner.height
    {
        return None;
    }
    let index = first + (row - list_y) as usize;
    (index < total).then_some(index)
}

fn picker_popup(area: Rect, app: &App) -> Rect {
    let name_width = picker_name_width(app);
    // " N  " + the padded name + "  " + the dim trail. The highlight is a row fill, not a
    // glyph, so it costs no width.
    let widest = app
        .picker_rows
        .iter()
        .map(|row| 4 + name_width + 2 + picker_trail(app, row).width())
        .max()
        .unwrap_or(0);
    menu_popup(area, app, widest, &picker_title(app), app.picker_rows.len() + 2)
}

/// The popup's row region, from the same `Block` shape the renderer draws, so the hit test
/// and the painted rows can never disagree about where a row starts.
fn picker_inner(popup: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(popup)
}

/// The names pad to the widest, so the dim tails start in one column.
fn picker_name_width(app: &App) -> usize {
    app.picker_rows.iter().map(|row| row.name.width()).max().unwrap_or(0)
}

/// A row's dim trail: the state, ` · <tab>` when herdr gave the tab a label, and ` · last used`
/// on the row of the agent this session last sent to — the remembered default reads before an
/// irreversible `enter` fires it (`specs/herdr-host.md`).
fn picker_trail(app: &App, row: &AgentChoice) -> String {
    let tab = if row.tab.is_empty() { String::new() } else { format!(" · {}", row.tab) };
    let last =
        if app.last_sent_pane.as_deref() == Some(&row.pane_id) { " · last used" } else { "" };
    format!("{}{tab}{last}", row.state)
}

fn picker_title(app: &App) -> String {
    let n = app.store.len();
    let noun = if n == 1 { "comment" } else { "comments" };
    format!("Send {n} {noun} to")
}

fn picker_scroll(app: &App, rows: usize) -> usize {
    menu_scroll(app.picker_cursor, app.picker_rows.len(), rows)
}

fn render_agent_picker(frame: &mut Frame, app: &App, area: Rect) {
    let p = app.palette();
    let popup = picker_popup(area, app);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.mauve))
        .title(framed_title(&picker_title(app)));
    let inner = picker_inner(popup);
    frame.render_widget(block, popup);

    let name_width = picker_name_width(app);
    let first = picker_scroll(app, inner.height as usize);
    let items: Vec<ListItem> = app
        .picker_rows
        .iter()
        .enumerate()
        .skip(first)
        .take(inner.height as usize)
        .map(|(i, row)| {
            // Only the first nine rows carry a number, since no digit key reaches further.
            let lead = if i < 9 { format!(" {}  ", i + 1) } else { "    ".to_string() };
            let pad = name_width.saturating_sub(row.name.width());
            let spans = vec![
                Span::styled(lead, Style::default().fg(p.overlay0)),
                // The name is the only part at full brightness: it is what the reviewer scans
                // for, and the two weights are what keep this a picker rather than a table.
                Span::styled(row.name.clone(), text_style(p)),
                Span::styled(
                    format!("{}  {}", " ".repeat(pad), picker_trail(app, row)),
                    Style::default().fg(p.overlay0),
                ),
            ];
            selectable_row(
                p,
                spans,
                inner.width as usize,
                (i == app.picker_cursor).then_some(p.surface2),
            )
        })
        .collect();
    frame.render_widget(List::new(items), inner);
}

/// The picker row under the pointer, for click-to-highlight (`specs/input.md`).
pub fn hit_picker_row(area: Rect, app: &App, col: u16, row: u16) -> Option<usize> {
    let inner = picker_inner(picker_popup(area, app));
    let first = picker_scroll(app, inner.height as usize);
    menu_hit(inner, 0, first, app.picker_rows.len(), col, row)
}

// --- Base picker (specs/input.md Base picker) ----------------------------------------------

/// A row's dim trail: `default` on the default branch, whose pick clears the record
/// (`specs/input.md` Base picker).
fn base_trail(row: &crate::app::BaseChoice) -> &'static str {
    if row.is_default { "default" } else { "" }
}

/// The names pad to the widest, so the dim `default` mark starts in one column.
fn base_name_width(bp: &crate::app::BasePicker) -> usize {
    bp.rows.iter().map(|r| r.name.width()).max().unwrap_or(0)
}

/// Sized like the agent picker's box, plus the filter line above the rows. The box holds its
/// full-list size while the filter narrows, so the frame never jumps under typing.
fn base_picker_popup(area: Rect, app: &App) -> Rect {
    let Some(bp) = &app.base_picker else { return Rect::default() };
    let name_width = base_name_width(bp);
    // The star lead + the padded name + the dim trail.
    let widest =
        bp.rows.iter().map(|r| 3 + name_width + 2 + base_trail(r).width()).max().unwrap_or(0);
    menu_popup(area, app, widest, BASE_PICKER_TITLE, bp.rows.len().max(1) + 3)
}

const BASE_PICKER_TITLE: &str = "Pick base branch";

fn base_picker_scroll(bp: &crate::app::BasePicker, rows: usize) -> usize {
    menu_scroll(bp.cursor, bp.filtered().len(), rows)
}

fn render_base_picker(frame: &mut Frame, app: &App, area: Rect) {
    let Some(bp) = &app.base_picker else { return };
    let p = app.palette();
    let popup = base_picker_popup(area, app);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.mauve))
        .title(framed_title(BASE_PICKER_TITLE));
    let inner = picker_inner(popup);
    frame.render_widget(block, popup);
    if inner.height == 0 {
        return;
    }

    // The filter line: the query with the comment editor's block caret, or a dim invitation
    // while it is empty. The single line cannot wrap, so it scrolls horizontally to keep the
    // caret in view — what was just typed stays visible (`specs/input.md` Base picker).
    let mut filter = if bp.query.is_empty() {
        let mut line = row_with_caret("", 0, p);
        line.spans.push(Span::styled(" type to filter…", Style::default().fg(p.overlay0)));
        line
    } else {
        let chars: Vec<char> = bp.query.chars().collect();
        let caret_col = bp.caret.min(chars.len());
        let avail = (inner.width as usize).saturating_sub(2);
        let start = caret_col.saturating_sub(avail.saturating_sub(1));
        let visible: String = chars[start.min(chars.len())..].iter().collect();
        row_with_caret(&visible, caret_col - start, p)
    };
    filter.spans.insert(0, Span::styled(" ", text_style(p)));
    frame.render_widget(Paragraph::new(filter), Rect { height: 1, ..inner });

    let list_area = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
    let filtered = bp.filtered();
    if filtered.is_empty() {
        let none = Line::from(Span::styled(" no branches match", Style::default().fg(p.overlay0)));
        frame.render_widget(Paragraph::new(none), list_area);
        return;
    }
    let name_width = base_name_width(bp);
    let first = base_picker_scroll(bp, list_area.height as usize);
    let items: Vec<ListItem> = filtered
        .iter()
        .enumerate()
        .skip(first)
        .take(list_area.height as usize)
        .map(|(vi, &ri)| {
            let row = &bp.rows[ri];
            // The star marks the open PR's target; the name is the only part at full
            // brightness, like the agent picker's rows (`specs/input.md`).
            let lead = if row.starred { " ★ " } else { "   " };
            let pad = name_width.saturating_sub(row.name.width());
            let spans = vec![
                Span::styled(lead.to_string(), Style::default().fg(p.yellow)),
                Span::styled(row.name.clone(), text_style(p)),
                Span::styled(
                    format!("{}  {}", " ".repeat(pad), base_trail(row)),
                    Style::default().fg(p.overlay0),
                ),
            ];
            selectable_row(
                p,
                spans,
                list_area.width as usize,
                (vi == bp.cursor).then_some(p.surface2),
            )
        })
        .collect();
    frame.render_widget(List::new(items), list_area);
}

/// The filtered base-picker row under the pointer, the filter line skipped
/// (`specs/input.md`).
pub fn hit_base_picker_row(area: Rect, app: &App, col: u16, row: u16) -> Option<usize> {
    let bp = app.base_picker.as_ref()?;
    let inner = picker_inner(base_picker_popup(area, app));
    let first = base_picker_scroll(bp, inner.height.saturating_sub(1) as usize);
    menu_hit(inner, 1, first, bp.filtered().len(), col, row)
}

/// The default body text color.
fn text_style(p: &Palette) -> Style {
    Style::default().fg(p.text)
}

/// A list row, highlighted with the shared selection fill (`surface2` + bold, full
/// width) when `selected` — the same treatment the diff cursor uses, so every cursor
/// in the UI reads the same. The fill is applied per span (with a trailing pad) so it
/// spans the full width under the `List` widget, matching the diff's `Paragraph` rows.
fn selectable_row(
    p: &Palette,
    mut spans: Vec<Span<'static>>,
    width: usize,
    fill: Option<Color>,
) -> ListItem<'static> {
    if let Some(bg) = fill {
        let used: usize = spans.iter().map(Span::width).sum();
        if width > used {
            spans.push(Span::raw(" ".repeat(width - used)));
        }
        for s in &mut spans {
            // A span with its own background keeps it; the rest take the selection fill.
            if s.style.bg.is_none() {
                s.style = s.style.bg(bg);
            }
            // Dim text lifts, so a selected row keeps its secondary parts: the file list's
            // indent, the picker row's state and tab trail.
            // The theme owns which color that is (`specs/theme.md`).
            if let Some(fg) = s.style.fg {
                s.style = s.style.fg(p.on_fill(fg));
            }
            s.style = s.style.add_modifier(Modifier::BOLD);
        }
    }
    ListItem::new(Line::from(spans))
}

// --- PR tab (specs/forge-host.md, specs/pr-tab.md) --------------------------------

/// The header for the read-only PR tab: the tab names, then a right-anchored, clickable
/// `status #number ↗` chip (status colored by lifecycle, the `↗` sharing the number's colour),
/// with the PR title right-aligned to its left. Merge/sync/checks live in the footer.
fn render_pr_header(frame: &mut Frame, app: &App, area: Rect) {
    let p = app.palette();
    let bar = Style::default().bg(p.surface0);
    let w = area.width as usize;
    // The chip's columns are reserved before the strip claims them: the strip shortens, the
    // chip stays (`tab_cap`). With no PR there is no chip, so the strip keeps the bar.
    let mut spans = tab_bar_spans_capped(app, tab_cap(app, area.width));
    let lead_tabs: usize = spans.iter().map(Span::width).sum();

    // A resolved PR shows its identity chip; with no PR the header carries nothing — the read
    // pane is the single home for the empty/degraded message, not repeated across all regions.
    if let forge::PrView::Pr(s) = &app.pr {
        let number = format!("{}{}", app.pr_forge.sigil(), s.number);
        let (status, color) = pr_status_chip(p, s);
        let chip_w = pr_chip_width(app, s);
        // The resolved head branch, dim left of the chip — the name that resolved, which can
        // differ from the worktree's local branch; `⑂` marks a fork head so a same-named
        // fork PR is visible (specs/forge-host.md). Dropped first when the bar is narrow.
        let head = match (s.head_ref.is_empty(), s.head_is_fork) {
            (true, _) => String::new(),
            (false, true) => format!("⑂ {}", s.head_ref),
            (false, false) => s.head_ref.clone(),
        };
        let head_w = if head.is_empty() { 0 } else { head.width() + 2 };
        // Keep the branch only while the title still gets a readable minimum beside it.
        let head_w =
            if w.saturating_sub(lead_tabs + chip_w + 2 + head_w) >= 8 { head_w } else { 0 };
        // The title fills the gap left of the branch + chip, right-aligned (a leading pad).
        // No floor: a bar with no room for a title shows none rather than overflowing the chip.
        let name = truncate_width(&s.title, w.saturating_sub(lead_tabs + chip_w + 2 + head_w));
        let pad = w.saturating_sub(lead_tabs + name.width() + head_w + 2 + chip_w);
        spans.push(Span::styled(" ".repeat(pad), bar));
        spans.push(Span::styled(name, bar.fg(p.subtext0)));
        if head_w > 0 {
            spans.push(Span::styled("  ", bar));
            spans.push(Span::styled(head, bar.fg(p.overlay0)));
        }
        spans.push(Span::styled("  ", bar));
        spans.push(Span::styled(status, bar.fg(color).add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(" ", bar));
        spans.push(Span::styled(number, bar.fg(p.yellow).add_modifier(Modifier::BOLD)));
        // The arrow shares the PR number's colour, reading as part of the clickable chip.
        spans.push(Span::styled(" ↗", bar.fg(p.yellow)));
    }

    // Fill the rest of the bar (the Pr arm already reaches the right edge).
    let used: usize = spans.iter().map(Span::width).sum();
    if used < w {
        spans.push(Span::styled(" ".repeat(w - used), bar));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The status chip word for a PR's lifecycle; its accent comes from [`pr_status_chip`].
fn pr_status_word(s: &forge::PrSnapshot) -> &'static str {
    match s.state {
        forge::PrState::Merged => "merged",
        forge::PrState::Closed => "closed",
        forge::PrState::Open if s.is_draft => "draft",
        forge::PrState::Open => "open",
    }
}

/// The status chip word and its theme accent, by lifecycle.
fn pr_status_chip(p: &Palette, s: &forge::PrSnapshot) -> (&'static str, Color) {
    let color = match s.state {
        forge::PrState::Merged => p.mauve,
        forge::PrState::Closed => p.red,
        forge::PrState::Open if s.is_draft => p.yellow,
        forge::PrState::Open => p.green,
    };
    (pr_status_word(s), color)
}

/// The display width of the header's `status #number ↗` chip — shared by the painter and the
/// click hit-test so they agree on its right-anchored column range.
fn pr_chip_width(app: &App, s: &forge::PrSnapshot) -> usize {
    pr_status_word(s).width()
        + " ".width()
        + format!("{}{}", app.pr_forge.sigil(), s.number).width()
        + " ↗".width()
}

/// The PR's merge, sync, and checks status for the footer, joined by `·`. Merge and sync show
/// only for an open PR — they are meaningless once it is merged or closed.
fn pr_state_line(app: &App, s: &forge::PrSnapshot) -> String {
    let mut parts: Vec<String> = Vec::new();
    if s.state == forge::PrState::Open {
        match s.merge {
            forge::Merge::Conflicting => parts.push(format!("⚠ conflicts with {}", s.base_ref)),
            forge::Merge::Blocked => parts.push("blocked".into()),
            forge::Merge::Clean => {}
        }
        match s.sync {
            forge::Sync::Unpushed(n) => parts.push(format!("⇡ {n} unpushed")),
            forge::Sync::Behind(n) => parts.push(format!("⇣ {n} behind")),
            forge::Sync::Unknown => parts.push("? sync unknown".to_string()),
            forge::Sync::InSync => {}
        }
    }
    parts.push(checks_summary(s));
    parts.push(format!("{} comments", s.comments.len()));
    // A capped surface means the lists are a prefix; point at the forge for the rest rather
    // than showing the partial counts as if complete (specs/forge-host.md).
    if s.truncated {
        parts.push(format!("+more on {} ↗", app.pr_forge.display_name()));
    }
    parts.join(" · ")
}

/// A one-token checks summary for the footer (`✓ checks` / `✗ N failing` / `● running`).
fn checks_summary(s: &forge::PrSnapshot) -> String {
    match s.checks_rollup() {
        None => "no checks".into(),
        Some(forge::CheckStatus::Failure) => format!("✗ {} failing", s.failing_checks()),
        Some(forge::CheckStatus::Running) => "● checks running".into(),
        Some(_) => "✓ checks".into(),
    }
}

/// The PR navigator: the checks list above the newest-first comments list, with the cursor
/// row filled and the view windowed to keep it on screen.
fn render_pr_nav(frame: &mut Frame, app: &App, area: Rect) {
    // Identity lives in the header; the read pane shows the selected comment, so the navigator
    // names its contents rather than repeating "PR".
    let p = app.palette();
    let block = bordered("Checks & comments", app.focus == Focus::Files, p);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width as usize;
    let rows = pr_nav_rows(app, width, std::time::SystemTime::now());
    let viewport = inner.height as usize;
    // Transitional frames retain the request until both a viewport and its selected row exist.
    let can_reveal = viewport > 0 && rows.iter().any(|row| row.cursor == Some(app.pr_cursor));
    let reveal = can_reveal && app.take_pr_nav_reveal();
    let (scroll, max_scroll) =
        settle_pr_nav_scroll(&rows, app.pr_cursor, viewport, app.pr_nav_scroll(), reveal);
    app.note_pr_nav_max_scroll(max_scroll);
    app.set_pr_nav_scroll(scroll);
    let items: Vec<ListItem> = rows
        .into_iter()
        .skip(scroll)
        .take(viewport)
        .map(|row| {
            let selected = row.cursor == Some(app.pr_cursor);
            selectable_row(p, row.spans, width, selected.then(|| p.cursor_bg(true)))
        })
        .collect();
    frame.render_widget(List::new(items), inner);
}

/// One painted PR navigator row and the cursor index it selects, when interactive.
struct PrNavRow {
    spans: Vec<Span<'static>>,
    cursor: Option<usize>,
}

/// The complete PR navigator layout, shared by painting and click hit-testing.
fn pr_nav_rows(app: &App, width: usize, now: std::time::SystemTime) -> Vec<PrNavRow> {
    let Some(s) = app.pr_snapshot() else { return Vec::new() };
    let p = app.palette();
    let dim = Style::default().fg(p.overlay0);
    let mut rows = Vec::new();
    if app.pr_has_description() {
        rows.push(PrNavRow {
            spans: vec![Span::styled("description", text_style(p))],
            cursor: Some(0),
        });
        rows.push(PrNavRow { spans: Vec::new(), cursor: None });
    }
    rows.push(PrNavRow { spans: vec![Span::styled(pr_checks_header(s), dim)], cursor: None });
    for check in &s.checks {
        let (glyph, color) = check_glyph(p, check.status);
        rows.push(PrNavRow {
            spans: vec![
                Span::styled(format!(" {glyph} "), Style::default().fg(color)),
                Span::styled(check.name.clone(), text_style(p)),
            ],
            cursor: None,
        });
    }
    rows.push(PrNavRow { spans: Vec::new(), cursor: None });
    rows.push(PrNavRow {
        spans: vec![Span::styled(format!("comments · {}", s.comments.len()), dim)],
        cursor: None,
    });
    let offset = app.pr_description_offset();
    rows.extend(s.comments.iter().enumerate().map(|(index, comment)| PrNavRow {
        spans: pr_comment_row(comment, width, now, p),
        cursor: Some(index + offset),
    }));
    rows
}

fn settle_pr_nav_scroll(
    rows: &[PrNavRow],
    cursor: usize,
    viewport: usize,
    current: usize,
    reveal: bool,
) -> (usize, usize) {
    let max = rows.len().saturating_sub(viewport);
    let mut scroll = current.min(max);
    if reveal && let Some(target) = rows.iter().position(|row| row.cursor == Some(cursor)) {
        if target < scroll {
            scroll = target;
        } else if target >= scroll.saturating_add(viewport) {
            scroll = target.saturating_add(1).saturating_sub(viewport);
        }
    }
    (scroll.min(max), max)
}

/// The `checks` section header with its rollup (`✗ 1 failing` / `✓ N passed` / `running`).
fn pr_checks_header(s: &forge::PrSnapshot) -> String {
    match s.checks_rollup() {
        None => "checks  none".into(),
        Some(forge::CheckStatus::Failure) => format!("checks  ✗ {} failing", s.failing_checks()),
        Some(forge::CheckStatus::Running) => "checks  running".into(),
        Some(_) => format!("checks  ✓ {} passed", s.checks.len()),
    }
}

/// One comment row: `@author anchor`, then a trailing `resolved`/`outdated` marker or the age.
fn pr_comment_row(
    cm: &forge::Comment,
    width: usize,
    now: std::time::SystemTime,
    p: &Palette,
) -> Vec<Span<'static>> {
    let author_color = if cm.author_is_bot { p.overlay1 } else { p.peach };
    let trailing = if cm.is_resolved {
        "resolved".to_string()
    } else if cm.is_outdated {
        "outdated".to_string()
    } else {
        forge::relative_age(&cm.created_at, now)
    };
    let author = format!("@{} ", cm.author);
    let budget = width.saturating_sub(author.width() + trailing.width() + 3).max(1);
    let anchor = elide_head(&cm.anchor, budget);
    vec![
        Span::styled(author, Style::default().fg(author_color)),
        Span::styled(anchor, text_style(p)),
        Span::styled(format!("  {trailing}"), Style::default().fg(p.overlay0)),
    ]
}

/// Note the painted link regions and heading anchors for a markdown render drawn
/// inside `inner`, scrolled by `scroll`, with the body's first line at display index
/// `offset` — so a click can resolve against exactly what this frame painted
/// (`specs/markdown.md`). Links note only the visible rows; anchors cover the whole
/// body, since an anchor click can jump past the viewport.
fn note_markdown_regions(
    app: &App,
    rendered: &crate::markdown::Rendered,
    inner: Rect,
    scroll: usize,
    offset: usize,
) {
    for (slug, line) in &rendered.anchors {
        app.note_painted_anchor(slug.clone(), line + offset);
    }
    let viewport = inner.height as usize;
    let visible = rendered.meta.iter().enumerate().filter_map(|(i, m)| {
        match (i + offset).checked_sub(scroll) {
            Some(d) if d < viewport => Some((d, m)),
            _ => None,
        }
    });
    for (display, m) in visible {
        for link in &m.links {
            let x1 = inner.x + link.start.min(inner.width as usize) as u16;
            let x2 = inner.x + link.end.min(inner.width as usize) as u16;
            if x1 < x2 {
                app.note_painted_link(x1, x2, inner.y + display as u16, link.url.clone());
            }
        }
    }
}

/// A ratatui scroll row from a usize offset, saturating — a render past 65k lines must
/// pin to the end, never wrap back near the top.
fn saturating_row(scroll: usize) -> u16 {
    u16::try_from(scroll).unwrap_or(u16::MAX)
}

/// A scrollbar in `track` when the content overflows the pane —
/// rendered markdown has no line numbers, so this is its position feedback
/// (`specs/diff-view.md`, `specs/pr-tab.md`). `max` is the maximum useful scroll; zero
/// (content fits) paints nothing.
fn render_overflow_scrollbar(
    frame: &mut Frame,
    track: Rect,
    max: usize,
    scroll: usize,
    p: &Palette,
) {
    if max == 0 {
        return;
    }
    let mut state = ScrollbarState::new(max).position(scroll);
    // A heavy-line accent thumb on the untouched border: the border thickens where the
    // reader is, and no track paints over it.
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("┃")
            .thumb_style(Style::default().fg(p.lavender)),
        track,
        &mut state,
    );
}

/// The PR read pane: the selected description or comment, or the loading/degraded message.
fn render_pr_read(frame: &mut Frame, app: &App, area: Rect) {
    let p = app.palette();
    let selected = app.pr_selected_comment();
    let title = match selected {
        Some(cm) => format!("@{} · {}", cm.author, cm.anchor),
        None if app.pr_on_description() => "description".to_string(),
        None => app.pr_forge.abbr().to_string(),
    };
    let block = bordered(&title, app.focus == Focus::Diff, p);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width as usize;
    let notice_lines =
        app.pr_notice().map(|notice| wrap_text(notice, width.max(1))).unwrap_or_default();
    // Keep one body row whenever the pane has room. If the remedy still cannot fit, retain its
    // opening state and actionable tail; the middle detail is less useful than a visible recovery.
    let notice_capacity = match inner.height {
        0 => 0,
        1 => 1,
        height => height - 1,
    } as usize;
    let notice_lines = if notice_lines.len() <= notice_capacity {
        notice_lines
    } else if notice_capacity == 0 {
        Vec::new()
    } else if notice_capacity == 1 {
        notice_lines.into_iter().rev().take(1).collect()
    } else {
        let tail = notice_lines.len() - (notice_capacity - 1);
        std::iter::once(notice_lines[0].clone())
            .chain(notice_lines.into_iter().skip(tail))
            .collect()
    };
    let notice_height = notice_lines.len() as u16;
    if notice_height > 0 {
        let notice_area = Rect::new(inner.x, inner.y, inner.width, notice_height);
        frame.render_widget(
            Paragraph::new(
                notice_lines
                    .into_iter()
                    .map(|line| Line::from(Span::styled(line, Style::default().fg(p.yellow))))
                    .collect::<Vec<_>>(),
            ),
            notice_area,
        );
    }
    let body = Rect::new(
        inner.x,
        inner.y.saturating_add(notice_height),
        inner.width,
        inner.height.saturating_sub(notice_height),
    );
    let mut lines: Vec<Line<'static>> = Vec::new();

    // The markdown body's render metadata and its first display row, for hit-testing.
    let mut body_meta: Option<(usize, crate::markdown::Rendered)> = None;
    if let Some(cm) = selected {
        // The finding's diff hunk stays plain `+`/`−`-colored lines; only the prose body
        // renders as markdown (specs/pr-tab.md).
        if let Some(hunk) = &cm.snippet {
            for raw in hunk.lines() {
                let color = match raw.bytes().next() {
                    Some(b'+') => p.green,
                    Some(b'-') => p.red,
                    _ => p.overlay0,
                };
                lines.push(Line::from(Span::styled(raw.to_string(), Style::default().fg(color))));
            }
            lines.push(Line::raw(""));
        }
        let mut rendered = app.markdown_render(&cm.body, width.max(1));
        let offset = lines.len();
        lines.append(&mut rendered.lines);
        body_meta = Some((offset, rendered));
        if cm.reply_count > 0 {
            let plural = if cm.reply_count == 1 { "reply" } else { "replies" };
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                format!(
                    "↳ {} {plural} — open on {} to read",
                    cm.reply_count,
                    app.pr_forge.display_name()
                ),
                Style::default().fg(p.overlay0),
            )));
        }
    } else if app.pr_on_description() {
        if let Some(s) = app.pr_snapshot() {
            let mut rendered = app.markdown_render(&s.body, width.max(1));
            let offset = lines.len();
            lines.append(&mut rendered.lines);
            body_meta = Some((offset, rendered));
        }
    } else {
        // The empty-state remedy can outgrow a narrow pane; wrap it rather than clip it.
        let refresh = app.keymap().hint(crate::keymap::Action::Refresh);
        for piece in wrap_text(&pr_empty_msg(&app.pr, app.pr_forge, refresh), width.max(1)) {
            lines.push(Line::from(Span::styled(piece, Style::default().fg(p.overlay0))));
        }
    }

    // Clamp in `usize` before the `u16` cast — a stale `pr_read_scroll` could otherwise
    // wrap below the clamp. Scrolling stops with the last line at the pane's bottom edge.
    let max = lines.len().saturating_sub(body.height as usize);
    app.note_pr_read_max_scroll(max);
    let scroll = app.pr_read_scroll.min(max);
    if let Some((offset, rendered)) = &body_meta {
        note_markdown_regions(app, rendered, body, scroll, *offset);
    }
    frame.render_widget(Paragraph::new(lines).scroll((saturating_row(scroll), 0)), body);
    render_overflow_scrollbar(
        frame,
        Rect::new(area.x, body.y, area.width, body.height),
        max,
        scroll,
        p,
    );
}

/// The one-line message for a loading, empty, or degraded PR view, in the resolved forge's
/// noun (`specs/forge-providers.md`). `refresh` is the active `refresh` binding's hint key.
fn pr_empty_msg(
    view: &forge::PrView,
    forge: crate::git::Forge,
    refresh: crate::keymap::Key,
) -> String {
    if let Some(message) = view.retry_remedy(refresh) {
        return message;
    }
    let noun = forge.noun();
    match view {
        forge::PrView::Loading => "loading…".into(),
        forge::PrView::Pending | forge::PrView::Pr(_) | forge::PrView::Held => String::new(),
        forge::PrView::Detached => format!("No {noun} found — HEAD is detached."),
        forge::PrView::NoPr => format!("No {noun} yet. Ready to ship?"),
        forge::PrView::NoCli(_)
        | forge::PrView::NoExtension(_)
        | forge::PrView::NotAuthed(..)
        | forge::PrView::GitError(_)
        | forge::PrView::Error(..) => {
            unreachable!("retry failures returned above")
        }
        forge::PrView::NeedsForgeRemote => {
            "The PR tab needs a GitHub, GitLab, or Azure DevOps remote named upstream or origin."
                .into()
        }
        forge::PrView::UnsupportedHost(host) => {
            format!(
                "Unsupported host: {host}. Self-hosted? Set `github_host`, `gitlab_host`, or `azure_devops_host`."
            )
        }
        forge::PrView::MalformedOrigin(host) => {
            format!("The origin remote must point to a repository path on {host}.")
        }
    }
}

/// Whether a click at `(col, row)` lands on the header's right-anchored `status #number ↗`
/// chip — the whole chip opens the PR.
#[must_use]
pub fn hit_pr_open(area: Rect, app: &App, col: u16, row: u16) -> bool {
    let Some(s) = app.pr_snapshot() else {
        return false;
    };
    if row != area.y {
        return false;
    }
    let chip_w = pr_chip_width(app, s) as u16;
    // The chip occupies the last `chip_w` columns; `saturating_sub` keeps the bound overflow-free.
    col >= area.width.saturating_sub(chip_w) && col < area.width
}

/// The cursor-row index a click at `(col, row)` lands on — the pinned description at the
/// row layout and the scroll captured by the painted frame.
#[must_use]
pub fn pr_nav_hit(area: Rect, app: &App, col: u16, row: u16) -> Option<usize> {
    let inner = inner_rect(panes(area, app).files);
    if !contains(inner, col, row) {
        return None;
    }
    let rows = pr_nav_rows(app, inner.width as usize, std::time::SystemTime::now());
    let scroll = app.pr_nav_scroll();
    rows.get((row - inner.y) as usize + scroll)?.cursor
}

/// The status glyph and Catppuccin accent for a check.
fn check_glyph(p: &Palette, status: forge::CheckStatus) -> (&'static str, Color) {
    match status {
        forge::CheckStatus::Success => ("✓", p.green),
        forge::CheckStatus::Failure => ("✗", p.red),
        forge::CheckStatus::Running => ("●", p.yellow),
        forge::CheckStatus::Pending => ("○", p.overlay0),
        forge::CheckStatus::Skipped => ("⊘", p.overlay0),
    }
}

// --- helpers -------------------------------------------------------------------

fn bordered(title: &str, focused: bool, p: &Palette) -> Block<'static> {
    // A focused pane gets a lavender border; an unfocused one recedes to a surface tone.
    let color = if focused { p.lavender } else { p.surface2 };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .title(framed_title(title))
}

/// Every block title breathes: one space each side, so the text never touches the border
/// run. One home for the affordance, so panes, the composer, and the popups agree.
fn framed_title(title: &str) -> String {
    if title.is_empty() { String::new() } else { format!(" {title} ") }
}

fn dim_paragraph<'a>(text: &'a str, p: &Palette) -> Paragraph<'a> {
    Paragraph::new(text).style(Style::default().fg(p.overlay0))
}

/// The theme accent for a change marker, matched to the diff's add/remove hues.
fn kind_color(p: &Palette, marker: char) -> Color {
    match marker {
        'A' | '?' => p.green,
        'D' => p.red,
        'R' => p.mauve,
        _ => p.yellow,
    }
}

/// Whether `(col, row)` falls inside `rect`.
fn contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// The content area inside a one-cell border.
fn inner_rect(outer: Rect) -> Rect {
    Rect {
        x: outer.x.saturating_add(1),
        y: outer.y.saturating_add(1),
        width: outer.width.saturating_sub(2),
        height: outer.height.saturating_sub(2),
    }
}

/// The linked-issues navigator: one row per issue the PR closes, newest number first.
/// Flat, unlike the PR navigator — there are no sections and every row is selectable.
fn render_issues_nav(frame: &mut Frame, app: &App, area: Rect) {
    let p = app.palette();
    let block = bordered("Linked issues", app.focus == Focus::Files, p);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width as usize;
    let viewport = inner.height as usize;
    let issues = app.issues();
    let rows = app.issues_rows();
    let max_scroll = rows.len().saturating_sub(viewport);
    app.note_issues_nav_max_scroll(max_scroll);
    // A cursor move asks for the smallest scroll that reveals the selection; the wheel does not.
    let scroll = if app.take_reveal_issues_nav() {
        let cursor = app.issues_cursor;
        let current = app.issues_nav_scroll().min(max_scroll);
        current.clamp(cursor.saturating_sub(viewport.saturating_sub(1)), cursor)
    } else {
        app.issues_nav_scroll().min(max_scroll)
    };
    app.set_issues_nav_scroll(scroll);
    let now = std::time::SystemTime::now();
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(viewport)
        .map(|(row_i, row)| {
            let selected = row_i == app.issues_cursor;
            let spans = match row {
                crate::app::IssueRow::Issue(i) => {
                    let issue = &issues[*i];
                    // A closed issue is dimmed rather than dropped: a PR that closes an
                    // already-closed issue is worth seeing, not hiding.
                    let state_color = match issue.state {
                        forge::IssueState::Open => p.green,
                        forge::IssueState::Closed => p.overlay0,
                    };
                    vec![
                        Span::styled(
                            format!("#{} ", issue.number),
                            Style::default().fg(p.overlay0),
                        ),
                        Span::raw(issue.title.clone()),
                        Span::styled(
                            format!(" {}", issue.state.label()),
                            Style::default().fg(state_color),
                        ),
                    ]
                }
                // Indented under its issue, `@author age` like a PR comment row. A human author
                // is emphasized over the bots, as on the PR tab.
                crate::app::IssueRow::Comment(i, c) => {
                    let cm = &issues[*i].comments[*c];
                    let author = if cm.author_is_bot { p.overlay0 } else { p.subtext0 };
                    vec![
                        Span::styled("  @", Style::default().fg(p.overlay0)),
                        Span::styled(cm.author.clone(), Style::default().fg(author)),
                        Span::styled(
                            format!("  {}", forge::relative_age(&cm.created_at, now)),
                            Style::default().fg(p.overlay0),
                        ),
                    ]
                }
            };
            selectable_row(p, spans, width, selected.then(|| p.cursor_bg(true)))
        })
        .collect();
    frame.render_widget(List::new(items), inner);
}

/// The selected issue's body as markdown, or the empty state when the PR closes none.
fn render_issues_read(frame: &mut Frame, app: &App, area: Rect) {
    let p = app.palette();
    let selected = app.issues_selected();
    let comment = app.issues_selected_comment();
    let title = match (comment, selected) {
        (Some(cm), Some(issue)) => format!("@{} · #{}", cm.author, issue.number),
        (_, Some(issue)) => format!("#{} {}", issue.number, issue.title),
        _ => app.pr_forge.abbr().to_string(),
    };
    let block = bordered(&title, app.focus == Focus::Diff, p);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut body_meta: Option<(usize, crate::markdown::Rendered)> = None;
    if let Some(cm) = comment {
        let mut rendered = app.markdown_render(&cm.body, width.max(1));
        let offset = lines.len();
        lines.append(&mut rendered.lines);
        body_meta = Some((offset, rendered));
    } else if let Some(issue) = selected {
        let mut rendered = app.markdown_render(&issue.body, width.max(1));
        let offset = lines.len();
        lines.append(&mut rendered.lines);
        body_meta = Some((offset, rendered));
    } else {
        let refresh = app.keymap().hint(crate::keymap::Action::Refresh);
        for piece in wrap_text(&issues_empty_msg(&app.pr, app.pr_forge, refresh), width.max(1)) {
            lines.push(Line::from(Span::styled(piece, Style::default().fg(p.overlay0))));
        }
    }
    let max = lines.len().saturating_sub(inner.height as usize);
    app.note_issues_read_max_scroll(max);
    let scroll = app.issues_read_scroll.min(max);
    if let Some((offset, rendered)) = &body_meta {
        note_markdown_regions(app, rendered, inner, scroll, *offset);
    }
    frame.render_widget(Paragraph::new(lines).scroll((saturating_row(scroll), 0)), inner);
    render_overflow_scrollbar(
        frame,
        Rect::new(area.x, inner.y, area.width, inner.height),
        max,
        scroll,
        p,
    );
}

/// The issues tab's empty states. A resolved PR that closes nothing is the tab's own message;
/// every other state is the PR tab's, so the two tabs never disagree about why they are blank.
fn issues_empty_msg(
    view: &forge::PrView,
    forge: crate::git::Forge,
    refresh: crate::keymap::Key,
) -> String {
    match view {
        forge::PrView::Pr(_) => {
            format!("No issues linked to this {}. Link one with \"Closes #n\".", forge.noun())
        }
        other => pr_empty_msg(other, forge, refresh),
    }
}
