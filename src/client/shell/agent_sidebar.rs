use std::collections::HashMap;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Paragraph, Widget},
};

use super::*;

pub(super) struct AgentRow {
    pub(super) pane_id: String,
    pub(super) status: crate::api::schema::AgentStatus,
    pub(super) focused: bool,
    pub(super) rows: Vec<Vec<crate::ui::ResolvedToken>>,
    /// Set while the person has marked this agent TODO.
    pub(super) todo: Option<AgentRowTodo>,
}

pub(super) struct AgentRowTodo {
    /// The pending reminder, already formatted in local time.
    pub(super) reminder: Option<String>,
}

/// Reads the TODO mark the server sends as reserved agent tokens.
pub(super) fn agent_todo(agent: &crate::protocol::ClientShellAgent) -> Option<AgentRowTodo> {
    let token = |name: &str| {
        agent
            .tokens
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    token(crate::pane_todo::TODO_TOKEN)?;
    let reminder = token(crate::pane_todo::REMIND_AT_TOKEN)
        .and_then(|value| value.parse::<u64>().ok())
        .map(|at| {
            crate::pane_todo::format_remind_at(
                at,
                crate::pane_todo::unix_now(),
                crate::pane_todo::cached_local_utc_offset_secs(),
            )
        });
    Some(AgentRowTodo { reminder })
}

/// Background of a TODO row: the palette's mauve mixed into the sidebar
/// background, stronger on the focused row.
pub(super) fn todo_row_bg(palette: &Palette, focused: bool) -> ratatui::style::Color {
    use ratatui::style::Color;

    let base = [palette.sidebar_bg, palette.panel_bg]
        .into_iter()
        .find_map(|color| match color {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            _ => None,
        })
        .unwrap_or((30, 30, 46));
    let Color::Rgb(r, g, b) = palette.mauve else {
        return palette.surface1;
    };
    let amount = if focused { 0.40 } else { 0.24 };
    let mix =
        |from: u8, to: u8| (f32::from(from) + (f32::from(to) - f32::from(from)) * amount) as u8;
    Color::Rgb(mix(base.0, r), mix(base.1, g), mix(base.2, b))
}

pub(super) fn ordered_agent_pane_ids(
    snapshot: &ClientShellSnapshot,
    sort: crate::config::AgentPanelSortConfig,
) -> Vec<String> {
    if snapshot.agent_view_label.is_some() {
        return snapshot
            .agent_order
            .iter()
            .filter(|pane_id| {
                snapshot
                    .agents
                    .iter()
                    .any(|agent| agent.pane_id == pane_id.as_str())
            })
            .cloned()
            .collect();
    }
    let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
    if sort == crate::config::AgentPanelSortConfig::Priority {
        agents.sort_by_key(|agent| {
            (
                std::cmp::Reverse(status_priority(agent.agent_status)),
                std::cmp::Reverse(agent.state_change_seq),
            )
        });
    }
    agents
        .into_iter()
        .map(|agent| agent.pane_id.clone())
        .collect()
}

/// One row of the agents list: a folder, or an agent at a folder depth.
enum AgentPanelRow {
    Folder(super::folders::FolderRow),
    Agent(AgentRow, u16),
}

pub(super) fn render_agent_panel(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    view: &super::render::SidebarFolderView<'_>,
    hits: &mut ShellHitMap,
) {
    if !render_agent_panel_header(
        buffer,
        area,
        snapshot.agent_view_label.as_deref(),
        config,
        hits,
    ) {
        return;
    }
    // A server-side agent view keeps its own order and stays flat.
    let folders = view.folders.filter(|_| snapshot.agent_view_label.is_none());
    if folders.is_some() {
        hits.agents_header = Rect::new(area.x, area.y + 1, area.width, 1);
        if view.drop_top_level {
            buffer.set_style(
                hits.agents_header,
                Style::default()
                    .bg(config.palette.surface1)
                    .fg(config.palette.accent),
            );
        }
    }

    let rows = match folders {
        Some(folders) => {
            let order = ordered_agent_pane_ids(snapshot, config.agent_panel_sort);
            super::folders::agent_list_rows(snapshot, &order, folders, &view.filter)
                .into_iter()
                .filter_map(|row| match row {
                    super::folders::AgentListRow::Folder(folder) => {
                        Some(AgentPanelRow::Folder(folder))
                    }
                    super::folders::AgentListRow::Agent { pane_id, depth } => {
                        agent_row(snapshot, &pane_id, config, None)
                            .map(|row| AgentPanelRow::Agent(row, depth))
                    }
                })
                .collect::<Vec<_>>()
        }
        None => agent_rows(snapshot, config, None)
            .into_iter()
            .map(|row| AgentPanelRow::Agent(row, 0))
            .collect(),
    };
    let empty_message = (snapshot.agent_view_label.is_some() || view.filter.is_active())
        .then_some(" no matching agents");
    render_agent_list(
        buffer,
        area,
        &rows,
        empty_message,
        config,
        agent_scroll,
        hits,
        |row| match row {
            AgentPanelRow::Folder(_) => 1,
            AgentPanelRow::Agent(row, _) => row.rows.len(),
        },
        |buffer, rect, row, hits| match row {
            AgentPanelRow::Folder(folder) => {
                render_folder_row(buffer, rect, folder, view, config);
                hits.folders.push(FolderHit {
                    rect,
                    section: super::folders::FolderSection::Agents,
                    path: folder.path.clone(),
                });
            }
            AgentPanelRow::Agent(row, depth) => {
                hits.agents.push((rect, row.pane_id.clone()));
                let dragged = view.dragged_agent == Some(row.pane_id.as_str());
                render_agent_row_at(buffer, rect, row, *depth, dragged, config);
            }
        },
    );
}

/// A folder row: ▾/▸, the folder's name, and on the right how many spaces or
/// agents it holds; a collapsed folder also shows the most urgent state inside
/// and a TODO mark when an agent inside has one.
pub(super) fn render_folder_row(
    buffer: &mut Buffer,
    rect: Rect,
    folder: &super::folders::FolderRow,
    view: &super::render::SidebarFolderView<'_>,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    if rect.is_empty() {
        return;
    }
    let drop_target = view.drop_folder == Some(folder.path.as_str());
    let dragged = view.dragged_folder == Some(folder.path.as_str());
    if drop_target {
        buffer.set_style(rect, Style::default().bg(palette.surface1));
    } else if dragged {
        buffer.set_style(rect, Style::default().bg(palette.surface0));
    }
    let indent = folder
        .depth
        .saturating_mul(2)
        .saturating_add(1)
        .min(rect.width.saturating_sub(4));
    let mut right = Vec::<(String, Style)>::new();
    if folder.collapsed && folder.count > 0 {
        right.push((
            status_icon(folder.status, config.status_indicators).to_owned(),
            Style::default().fg(status_color(folder.status, palette)),
        ));
    }
    if folder.count > 0 {
        right.push((
            folder.count.to_string(),
            Style::default().fg(palette.overlay0),
        ));
    }
    let right_width = right
        .iter()
        .map(|(text, _)| display_width(text) + 1)
        .sum::<usize>() as u16;
    let name_style = Style::default()
        .fg(if drop_target {
            palette.text
        } else {
            palette.accent
        })
        .add_modifier(Modifier::BOLD);
    let x = rect.x + indent;
    put_text(
        buffer,
        x,
        rect.y,
        2,
        if folder.collapsed { "▸ " } else { "▾ " },
        name_style,
    );
    let name_width = rect
        .right()
        .saturating_sub(x + 2)
        .saturating_sub(right_width + 1);
    put_text(
        buffer,
        x + 2,
        rect.y,
        name_width,
        super::folders::folder_name(&folder.path),
        name_style,
    );
    if right_width + 6 <= rect.width {
        let mut right_x = rect.right().saturating_sub(right_width);
        for (text, style) in right {
            let width = display_width(&text) as u16;
            put_text(buffer, right_x, rect.y, width, &text, style);
            right_x += width + 1;
        }
    }
    if folder.collapsed && folder.todo {
        put_text(
            buffer,
            rect.x,
            rect.y,
            1,
            "▌",
            Style::default()
                .fg(palette.mauve)
                .add_modifier(Modifier::BOLD),
        );
    }
}

pub(super) fn render_agent_panel_header(
    buffer: &mut Buffer,
    area: Rect,
    agent_view_label: Option<&str>,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) -> bool {
    if area.height == 0 {
        return false;
    }
    put_text(
        buffer,
        area.x,
        area.y,
        area.width,
        &"─".repeat(area.width as usize),
        Style::default().fg(config.palette.surface_dim),
    );
    if area.height < 2 {
        return false;
    }
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        " agents",
        Style::default()
            .fg(config.palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );
    let sort_label = agent_view_label.unwrap_or(match config.agent_panel_sort {
        crate::config::AgentPanelSortConfig::Spaces => "grouped",
        crate::config::AgentPanelSortConfig::Priority => "priority",
    });
    let sort_width = display_width(sort_label).min(area.width as usize) as u16;
    let sort_rect = Rect::new(
        area.right().saturating_sub(sort_width),
        area.y + 1,
        sort_width,
        1,
    );
    hits.agent_sort_toggle = if config.mouse_capture && agent_view_label.is_none() {
        sort_rect
    } else {
        Rect::default()
    };
    put_text(
        buffer,
        sort_rect.x,
        sort_rect.y,
        sort_rect.width,
        sort_label,
        Style::default()
            .fg(if agent_view_label.is_some() {
                config.palette.accent
            } else {
                config.palette.overlay0
            })
            .add_modifier(Modifier::BOLD),
    );
    true
}

pub(super) fn render_agent_list<T>(
    buffer: &mut Buffer,
    area: Rect,
    rows: &[T],
    empty_message: Option<&str>,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
    row_lines: impl Fn(&T) -> usize,
    mut render_row: impl FnMut(&mut Buffer, Rect, &T, &mut ShellHitMap),
) {
    let body = Rect::new(
        area.x,
        area.y.saturating_add(3),
        area.width,
        area.height.saturating_sub(3),
    );
    hits.agent_body = body;
    if body.is_empty() || rows.is_empty() {
        *agent_scroll = 0;
        if let Some(message) = empty_message.filter(|_| !body.is_empty()) {
            put_text(
                buffer,
                body.x,
                body.y,
                body.width,
                message,
                Style::default()
                    .fg(config.palette.overlay0)
                    .add_modifier(Modifier::DIM),
            );
        }
        return;
    }

    let row_heights = rows
        .iter()
        .map(|row| row_lines(row).max(1).min(u16::MAX as usize) as u16)
        .collect::<Vec<_>>();
    let gaps = rows
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if index + 1 < rows.len() {
                config.agents.row_gap
            } else {
                0
            }
        })
        .collect::<Vec<_>>();
    let metrics =
        super::scroll::list_scroll_metrics(&row_heights, &gaps, body.height, *agent_scroll);
    hits.agent_max_scroll = metrics.max_offset_from_bottom;
    hits.agent_scroll_metrics = Some(metrics);
    *agent_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    let mut y = body.y;
    for (index, row) in rows.iter().enumerate().skip(*agent_scroll) {
        let height = row_heights[index].min(body.height);
        if y.saturating_add(height) > body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, height);
        render_row(buffer, rect, row, hits);
        y = y
            .saturating_add(height)
            .saturating_add(if index + 1 < rows.len() {
                config.agents.row_gap
            } else {
                0
            });
    }

    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.agent_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, &config.palette);
    }
}

pub(super) fn agent_rows(
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    machine: Option<&str>,
) -> Vec<AgentRow> {
    ordered_agent_pane_ids(snapshot, config.agent_panel_sort)
        .into_iter()
        .filter_map(|pane_id| agent_row(snapshot, &pane_id, config, machine))
        .collect()
}

pub(super) fn agent_row(
    snapshot: &ClientShellSnapshot,
    pane_id: &str,
    config: &ClientShellConfig,
    machine: Option<&str>,
) -> Option<AgentRow> {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.pane_id == pane_id)?;
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)?;
    let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == agent.tab_id);
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == agent.pane_id);
    let tab_count = snapshot
        .tabs
        .iter()
        .filter(|candidate| candidate.workspace_id == agent.workspace_id)
        .count();
    let tab_label = tab
        .filter(|tab| tab_count > 1 || tab.custom_label)
        .map(|tab| tab.label.as_str());
    let agent_label = agent
        .display_agent
        .as_deref()
        .or(agent.name.as_deref())
        .or(agent.agent.as_deref())
        .or(agent.title.as_deref());
    let labels = agent
        .state_labels
        .iter()
        .cloned()
        .collect::<HashMap<_, _>>();
    let tokens = agent.tokens.iter().cloned().collect::<HashMap<_, _>>();
    let state_text = labels
        .get(status_text(agent.agent_status))
        .map(String::as_str)
        .unwrap_or_else(|| sidebar_status_text(agent.agent_status));
    let canonical_agent = agent
        .agent
        .as_deref()
        .and_then(crate::detect::parse_agent_label);
    let rows = crate::ui::sidebar_agent_rows(
        &config.agents,
        crate::ui::AgentTokenContext {
            machine,
            workspace: &workspace.label,
            tab: tab_label,
            pane: agent
                .title
                .as_deref()
                .or_else(|| pane.and_then(|pane| pane.label.as_deref())),
            agent_label,
            terminal_title: agent.terminal_title.as_deref(),
            terminal_title_stripped: agent.terminal_title_stripped.as_deref(),
            canonical_agent,
            tokens: &tokens,
        },
        state_text,
    );
    Some(AgentRow {
        pane_id: agent.pane_id.clone(),
        status: agent.agent_status,
        focused: agent.focused,
        rows,
        todo: agent_todo(agent),
    })
}

pub(super) fn render_agent_row(
    buffer: &mut Buffer,
    rect: Rect,
    row: &AgentRow,
    config: &ClientShellConfig,
) {
    render_agent_row_at(buffer, rect, row, 0, false, config);
}

/// An agent row indented to its folder depth.
pub(super) fn render_agent_row_at(
    buffer: &mut Buffer,
    rect: Rect,
    row: &AgentRow,
    depth: u16,
    dragged: bool,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    let depth_indent = (depth as usize * 2).min((rect.width as usize).saturating_sub(8));
    let row_style = if dragged {
        Style::default().bg(palette.surface1)
    } else if row.todo.is_some() {
        Style::default().bg(todo_row_bg(palette, row.focused))
    } else if row.focused {
        Style::default().bg(palette.active_row_bg)
    } else {
        Style::default()
    };
    let name_style = if row.focused {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD)
    };
    let status_style = Style::default().fg(status_color(row.status, palette));
    let secondary = Style::default().fg(palette.overlay0);
    let icon = (
        status_icon(row.status, config.status_indicators),
        Style::default().fg(status_color(row.status, palette)),
    );
    let rows = if row.rows.is_empty() {
        vec![vec![crate::ui::ResolvedToken {
            kind: crate::ui::ResolvedTokenKind::StateIcon,
            style: Default::default(),
        }]]
    } else {
        row.rows.clone()
    };
    let line_count = rows.len().min(rect.height as usize);
    let reminder = row
        .todo
        .as_ref()
        .and_then(|todo| todo.reminder.as_deref())
        .map(|reminder| format!("◷ {reminder} "));
    for (index, tokens) in rows.iter().take(line_count).enumerate() {
        let indent = depth_indent + if index == 0 { 1 } else { 3 };
        // The reminder sits at the right end of the last line.
        let badge = reminder
            .as_deref()
            .filter(|_| index + 1 == line_count)
            .filter(|badge| display_width(badge) + indent + 4 <= rect.width as usize);
        let badge_width = badge.map_or(0, display_width) as u16;
        let mut spans = vec![ratatui::text::Span::raw(" ".repeat(indent))];
        spans.extend(crate::ui::resolved_token_spans(
            tokens,
            icon,
            status_style,
            name_style,
            secondary,
            secondary,
            palette,
            rect.width.saturating_sub(indent as u16 + badge_width) as usize,
        ));
        let y = rect.y + index as u16;
        Paragraph::new(Line::from(spans))
            .style(row_style)
            .render(Rect::new(rect.x, y, rect.width, 1), buffer);
        if row.todo.is_none() {
            continue;
        }
        let mark = row_style.fg(palette.mauve).add_modifier(Modifier::BOLD);
        put_text(buffer, rect.x, y, 1, "▌", mark);
        if let Some(badge) = badge {
            put_text(
                buffer,
                rect.right().saturating_sub(badge_width),
                y,
                badge_width,
                badge,
                mark,
            );
        }
    }
}

fn put_text(buffer: &mut Buffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    for (offset, character) in text.chars().take(width as usize).enumerate() {
        if let Some(cell) = buffer.cell_mut((x + offset as u16, y)) {
            cell.set_char(character).set_style(style);
        }
    }
}

fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

fn sidebar_status_text(status: crate::api::schema::AgentStatus) -> &'static str {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Working => "working",
        AgentStatus::Idle | AgentStatus::Unknown => "idle",
    }
}
