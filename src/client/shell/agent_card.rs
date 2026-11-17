//! The card a Claude agent's row shows while the pointer rests on it: what the
//! agent is, its login, model and context, a limit that holds it, and the end
//! of its last message, so a person sees why it waits without switching to it.
//! It only shows; answering happens in the agent's own tab.

use std::time::{Duration, Instant};

use crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::agent_insight_view::{self as view, AgentInsightView};
use super::*;

/// How long the pointer rests on a row before its card shows.
const SHOW_AFTER: Duration = Duration::from_millis(450);
const CARD_WIDTH: u16 = 64;
const EXCERPT_LINES: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentHover {
    pub(super) pane_id: String,
    since: Instant,
    pub(super) shown: bool,
}

impl ClientShellState {
    /// Follows the pointer over agent rows. True when a shown card changed.
    pub(super) fn track_agent_hover(&mut self, mouse: &MouseEvent) -> bool {
        let point = (mouse.column, mouse.row);
        let over = (mouse.kind == MouseEventKind::Moved && self.overlay.is_none())
            .then(|| {
                self.hits
                    .agents
                    .iter()
                    .find(|(rect, _)| contains(*rect, point))
                    .map(|(_, pane_id)| pane_id.clone())
            })
            .flatten();
        let Some(pane_id) = over else {
            return self.hide_agent_hover();
        };
        if self
            .agent_hover
            .as_ref()
            .is_some_and(|hover| hover.pane_id == pane_id)
        {
            return false;
        }
        // Moving from one card's row to the next shows the next at once.
        let shown = self.agent_hover.as_ref().is_some_and(|hover| hover.shown);
        self.agent_hover = Some(AgentHover {
            pane_id,
            since: Instant::now(),
            shown,
        });
        shown
    }

    /// Drops the card, as any click, key or scroll does. True when one showed.
    pub(super) fn hide_agent_hover(&mut self) -> bool {
        self.agent_hover.take().is_some_and(|hover| hover.shown)
    }

    pub(crate) fn tick_agent_hover(&mut self, now: Instant) -> bool {
        match self.agent_hover.as_mut() {
            Some(hover) if !hover.shown && now.duration_since(hover.since) >= SHOW_AFTER => {
                hover.shown = true;
                true
            }
            _ => false,
        }
    }
}

/// Draws the card beside `row`. None when there is nothing to show: the agent
/// is not a Claude session the server reads, or the screen has no room.
pub(super) fn render_agent_card(
    buffer: &mut Buffer,
    row: Rect,
    agent: &crate::protocol::ClientShellAgent,
    snapshot: &ClientShellSnapshot,
    palette: &Palette,
) -> Option<Rect> {
    let insight = AgentInsightView::of(agent);
    if !insight.known {
        return None;
    }
    let screen = buffer.area;
    let width = CARD_WIDTH.min(screen.width);
    if width < 24 {
        return None;
    }
    let inner_width = usize::from(width - 4);
    let lines = card_lines(agent, snapshot, &insight, inner_width, palette);
    let height = (lines.len() as u16 + 2).min(screen.height);
    let x = row
        .right()
        .saturating_add(1)
        .min(screen.right().saturating_sub(width));
    let y = row.y.min(screen.bottom().saturating_sub(height));
    let rect = Rect::new(x, y, width, height);
    draw_box(buffer, rect, palette);
    for (index, line) in lines.iter().enumerate() {
        let line_y = rect.y + 1 + index as u16;
        if line_y + 1 >= rect.bottom() {
            break;
        }
        let mut line_x = rect.x + 2;
        for (text, style) in line {
            let room = (rect.right() - 2).saturating_sub(line_x);
            let drawn = put_clipped(buffer, line_x, line_y, room, text, *style);
            line_x += drawn;
        }
    }
    Some(rect)
}

type CardLine = Vec<(String, Style)>;

fn card_lines(
    agent: &crate::protocol::ClientShellAgent,
    snapshot: &ClientShellSnapshot,
    insight: &AgentInsightView,
    width: usize,
    palette: &Palette,
) -> Vec<CardLine> {
    let text = Style::default().fg(palette.text).bg(palette.panel_bg);
    let dim = Style::default().fg(palette.overlay0).bg(palette.panel_bg);
    let mut lines = vec![vec![(
        card_title(agent, snapshot),
        text.add_modifier(Modifier::BOLD),
    )]];

    let statuses = view::login_statuses(&snapshot.agents);
    let login_status = insight
        .login
        .as_deref()
        .and_then(|login| statuses.iter().find(|status| status.login == login));
    let mut meta = Vec::new();
    if let Some(login) = &insight.login {
        meta.push(view::short_login(login).to_owned());
    }
    if let Some(model) = &insight.model {
        meta.push(view::short_model(model));
    }
    if let Some(context) = insight.context {
        meta.push(format!("ctx {}", view::format_tokens(context)));
    }
    if let Some(status) = login_status {
        if let Some(percent) = status.five_hour_percent {
            meta.push(format!("5h {percent}%"));
        }
        if let Some(percent) = status.week_percent {
            meta.push(format!("wk {percent}%"));
        }
    }
    if !meta.is_empty() {
        lines.push(vec![(meta.join(" · "), dim)]);
    }

    let peach = Style::default().fg(palette.peach).bg(palette.panel_bg);
    match insight.limit {
        Some(crate::agent_insight::LimitHit::Login) => lines.push(vec![(
            format!(
                "{} login expired: /login there, or relaunch under another",
                super::agent_sidebar::LIMIT_ICON
            ),
            peach,
        )]),
        Some(hit) => {
            let mut line = match hit {
                crate::agent_insight::LimitHit::Usage { resets_at: Some(_) } => format!(
                    "{} limit until {}",
                    super::agent_sidebar::LIMIT_ICON,
                    view::limit_label(hit)
                ),
                _ => format!(
                    "{} limit hit, reset time unknown",
                    super::agent_sidebar::LIMIT_ICON
                ),
            };
            if insight.continue_after_reset {
                line.push_str(" · continues after reset");
            }
            lines.push(vec![(line, peach)]);
        }
        None => {}
    }

    if let Some(excerpt) = insight
        .excerpt
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        lines.push(Vec::new());
        if insight.asks {
            lines.push(vec![(
                format!("{} asks you", super::agent_sidebar::QUESTION_ICON),
                Style::default()
                    .fg(palette.red)
                    .bg(palette.panel_bg)
                    .add_modifier(Modifier::BOLD),
            )]);
        }
        let mut wrapped = wrap(excerpt, width);
        // The end of a message holds the question: keep it when cutting.
        if wrapped.len() > EXCERPT_LINES {
            wrapped.drain(..wrapped.len() - EXCERPT_LINES);
            wrapped[0] = format!("… {}", wrapped[0]);
        }
        lines.extend(wrapped.into_iter().map(|line| vec![(line, text)]));
    }
    lines
}

/// "space · tab · pane", each part once. The terminal title stands in only
/// when the tab and the pane have no name: once they do, it is mostly the
/// shell's "user@host: ~".
fn card_title(agent: &crate::protocol::ClientShellAgent, snapshot: &ClientShellSnapshot) -> String {
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)
        .map(|workspace| workspace.label.as_str());
    let tab = snapshot
        .tabs
        .iter()
        .find(|tab| tab.tab_id == agent.tab_id && tab.custom_label)
        .map(|tab| tab.label.as_str());
    let pane = agent.title.as_deref().or_else(|| {
        snapshot
            .panes
            .iter()
            .find(|pane| pane.pane_id == agent.pane_id)
            .and_then(|pane| pane.label.as_deref())
    });
    let fallback = (tab.is_none() && pane.is_none())
        .then(|| {
            agent
                .terminal_title_stripped
                .as_deref()
                .or(agent.display_agent.as_deref())
        })
        .flatten();
    let mut parts = Vec::<&str>::new();
    for part in [workspace, tab, pane, fallback].into_iter().flatten() {
        let part = part.trim();
        if !part.is_empty() && !parts.contains(&part) {
            parts.push(part);
        }
    }
    parts.join(" · ")
}

/// Wraps by words to `width` columns, paragraphs kept apart by a blank line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines = Vec::new();
    for (index, paragraph) in text.split("\n\n").enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        for source_line in paragraph.lines() {
            let mut line = String::new();
            let mut line_width = 0;
            for word in source_line.split_whitespace() {
                let mut word = word.to_owned();
                loop {
                    let word_width = unicode_width::UnicodeWidthStr::width(word.as_str());
                    let gap = usize::from(line_width > 0);
                    if line_width + gap + word_width <= width {
                        if gap == 1 {
                            line.push(' ');
                        }
                        line.push_str(&word);
                        line_width += gap + word_width;
                        break;
                    }
                    if line_width > 0 {
                        lines.push(std::mem::take(&mut line));
                        line_width = 0;
                        continue;
                    }
                    // A word longer than the line is cut.
                    let (head, tail) = split_at_width(&word, width);
                    lines.push(head);
                    word = tail;
                    if word.is_empty() {
                        break;
                    }
                }
            }
            lines.push(line);
        }
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

fn split_at_width(word: &str, width: usize) -> (String, String) {
    let mut taken = 0;
    for (index, character) in word.char_indices() {
        let character_width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if taken + character_width > width {
            return (word[..index].to_owned(), word[index..].to_owned());
        }
        taken += character_width;
    }
    (word.to_owned(), String::new())
}

fn draw_box(buffer: &mut Buffer, rect: Rect, palette: &Palette) {
    let background = Style::default().bg(palette.panel_bg);
    let border = Style::default().fg(palette.surface1).bg(palette.panel_bg);
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            let last_x = x + 1 == rect.right();
            let last_y = y + 1 == rect.bottom();
            let symbol = match (x == rect.x, last_x, y == rect.y, last_y) {
                (true, _, true, _) => "╭",
                (_, true, true, _) => "╮",
                (true, _, _, true) => "╰",
                (_, true, _, true) => "╯",
                (true, _, _, _) | (_, true, _, _) => "│",
                (_, _, true, _) | (_, _, _, true) => "─",
                _ => " ",
            };
            let style = if symbol == " " { background } else { border };
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_symbol(symbol).set_style(style);
            }
        }
    }
}

/// Writes `text` from `x`, at most `room` columns of it. Returns the columns used.
fn put_clipped(buffer: &mut Buffer, x: u16, y: u16, room: u16, text: &str, style: Style) -> u16 {
    let mut used = 0u16;
    for character in text.chars() {
        let width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0) as u16;
        if width == 0 {
            continue;
        }
        if used + width > room {
            break;
        }
        if let Some(cell) = buffer.cell_mut((x + used, y)) {
            cell.set_char(character).set_style(style);
        }
        // A wide character's second cell is left for it to cover.
        if width == 2 {
            if let Some(cell) = buffer.cell_mut((x + used + 1, y)) {
                cell.set_symbol("").set_style(style);
            }
        }
        used += width;
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_by_words_and_keeps_paragraphs() {
        assert_eq!(
            wrap("one two three four\n\nfive", 9),
            ["one two", "three", "four", "", "five"]
        );
        assert_eq!(wrap("abcdefghijkl", 8), ["abcdefgh", "ijkl"]);
    }
}
