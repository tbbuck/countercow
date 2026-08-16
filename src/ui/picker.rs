//! The startup screen: choose a .NET process to attach to.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::ipc::commands::ProcessInfo;
use crate::ipc::discovery::{Discovery, DotnetProcess};

use super::theme::Theme;

pub struct Entry {
    pub process: DotnetProcess,
    /// Best-effort: a process can exit between discovery and the identity query.
    pub info: Option<ProcessInfo>,
}

impl Entry {
    /// The name worth showing.
    ///
    /// Discovery often yields "dotnet" for framework-dependent apps launched via the host, but
    /// the runtime knows its own entry assembly, which is what a person is actually looking for.
    pub fn display_name(&self) -> &str {
        let assembly = self
            .info
            .as_ref()
            .and_then(|i| i.assembly_name.as_deref())
            .filter(|name| !name.is_empty());

        match assembly {
            Some(assembly) if self.process.name == "dotnet" => assembly,
            _ => &self.process.name,
        }
    }

    pub fn runtime(&self) -> String {
        self.info
            .as_ref()
            .and_then(ProcessInfo::framework_label)
            .unwrap_or_else(|| "—".into())
    }
}

pub struct Picker {
    pub entries: Vec<Entry>,
    pub skipped: Skipped,
    state: TableState,
    pub cancelled: bool,
}

/// What discovery deliberately left out, so the picker can say so rather than appear incomplete.
pub struct Skipped {
    pub stale: usize,
    pub mismatched: usize,
    pub foreign: usize,
    pub too_long: usize,
}

impl Skipped {
    fn total(&self) -> usize {
        self.stale + self.mismatched + self.foreign + self.too_long
    }

    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.stale > 0 {
            parts.push(format!("{} stale", self.stale));
        }
        if self.mismatched > 0 {
            parts.push(format!("{} pid-reused", self.mismatched));
        }
        if self.foreign > 0 {
            parts.push(format!("{} other users", self.foreign));
        }
        if self.too_long > 0 {
            parts.push(format!("{} path too long", self.too_long));
        }
        format!("{} sockets skipped ({})", self.total(), parts.join(", "))
    }
}

impl Picker {
    pub fn new(discovery: Discovery, entries: Vec<Entry>) -> Self {
        let mut state = TableState::default();
        if !entries.is_empty() {
            state.select(Some(0));
        }
        Self {
            entries,
            skipped: Skipped {
                stale: discovery.stale,
                mismatched: discovery.mismatched,
                foreign: discovery.foreign,
                too_long: discovery.too_long,
            },
            state,
            cancelled: false,
        }
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.state.selected()?)
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        let current = self.state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, last as isize) as usize;
        self.state.select(Some(next));
    }

    pub fn select_first(&mut self) {
        if !self.entries.is_empty() {
            self.state.select(Some(0));
        }
    }

    pub fn select_last(&mut self) {
        if !self.entries.is_empty() {
            self.state.select(Some(self.entries.len() - 1));
        }
    }

    pub fn render(&mut self, frame: &mut Frame, theme: &Theme) {
        let area = frame.area();
        let [title_area, body_area, footer_area] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Fill(1),
            Constraint::Length(2),
        ])
        .areas(area);

        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::from("countercow").fg(theme.accent).bold()),
                Line::from(
                    Span::from("select a .NET process to watch").fg(theme.dim),
                ),
            ]),
            title_area,
        );

        if self.entries.is_empty() {
            self.render_empty(frame, body_area, theme);
        } else {
            self.render_table(frame, body_area, theme);
        }

        let mut footer = vec![Line::from(vec![
            Span::from(" ↑↓").fg(theme.accent),
            Span::from(" move  ").fg(theme.dim),
            Span::from("enter").fg(theme.accent),
            Span::from(" attach  ").fg(theme.dim),
            Span::from("q").fg(theme.accent),
            Span::from(" quit").fg(theme.dim),
        ])];
        if self.skipped.total() > 0 {
            footer.push(Line::from(
                Span::from(format!(" {}", self.skipped.describe())).fg(theme.dim),
            ));
        }
        frame.render_widget(Paragraph::new(footer), footer_area);
    }

    fn render_empty(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let mut lines = vec![
            Line::from(Span::from("No attachable .NET processes found.").fg(theme.warn)),
            Line::from(""),
            Line::from(Span::from("A process is attachable when it is running a .NET").fg(theme.dim)),
            Line::from(Span::from("runtime with diagnostics enabled, and belongs to you.").fg(theme.dim)),
        ];
        if self.skipped.foreign > 0 {
            lines.push(Line::from(""));
            lines.push(Line::from(
                Span::from(format!(
                    "{} belong to another user and cannot be reached.",
                    self.skipped.foreign
                ))
                .fg(theme.dim),
            ));
        }

        let block = Block::bordered().border_style(Style::default().fg(theme.border));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_table(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let rows: Vec<Row> = self
            .entries
            .iter()
            .map(|entry| {
                Row::new(vec![
                    Line::from(Span::from(entry.process.pid.to_string()).fg(theme.dim))
                        .right_aligned(),
                    Line::from(Span::from(entry.display_name().to_owned()).fg(theme.fg)),
                    Line::from(Span::from(entry.runtime()).fg(theme.dim)),
                    Line::from(Span::from(entry.process.command.clone()).fg(theme.dim)),
                ])
            })
            .collect();

        let header = Row::new(vec!["PID", "NAME", "RUNTIME", "COMMAND"])
            .style(Style::default().fg(theme.title).bold());

        let table = Table::new(
            rows,
            [
                Constraint::Length(7),
                Constraint::Length(26),
                Constraint::Length(9),
                Constraint::Fill(1),
            ],
        )
        .header(header)
        .column_spacing(2)
        .row_highlight_style(Style::default().fg(theme.accent).reversed())
        .highlight_symbol("")
        .block(Block::bordered().border_style(Style::default().fg(theme.border)));

        frame.render_stateful_widget(table, area, &mut self.state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn process(pid: u32, name: &str) -> DotnetProcess {
        DotnetProcess {
            pid,
            socket: PathBuf::from("/tmp/s"),
            name: name.into(),
            command: "cmd".into(),
            start_key_verified: true,
        }
    }

    fn entry(pid: u32, name: &str, assembly: Option<&str>) -> Entry {
        Entry {
            process: process(pid, name),
            info: assembly.map(|a| ProcessInfo {
                assembly_name: Some(a.into()),
                clr_version: Some("9.0.7".into()),
                ..Default::default()
            }),
        }
    }

    fn picker(entries: Vec<Entry>) -> Picker {
        Picker::new(Discovery::default(), entries)
    }

    #[test]
    fn prefers_the_entry_assembly_when_the_process_is_just_dotnet() {
        let entry = entry(1, "dotnet", Some("CrimeRate.Front"));
        assert_eq!(entry.display_name(), "CrimeRate.Front");
        assert_eq!(entry.runtime(), "net9.0");
    }

    #[test]
    fn keeps_a_real_executable_name_over_the_assembly_name() {
        let entry = entry(1, "Rider.Backend", Some("Rider.Backend"));
        assert_eq!(entry.display_name(), "Rider.Backend");
    }

    #[test]
    fn falls_back_when_identity_is_unavailable() {
        let entry = entry(1, "dotnet", None);
        assert_eq!(entry.display_name(), "dotnet");
        assert_eq!(entry.runtime(), "—");
    }

    #[test]
    fn selection_starts_at_the_first_entry_and_clamps() {
        let mut picker = picker(vec![entry(1, "a", None), entry(2, "b", None)]);
        assert_eq!(picker.selected().unwrap().process.pid, 1);

        picker.move_by(1);
        assert_eq!(picker.selected().unwrap().process.pid, 2);
        picker.move_by(5);
        assert_eq!(picker.selected().unwrap().process.pid, 2, "clamped at the end");
        picker.move_by(-99);
        assert_eq!(picker.selected().unwrap().process.pid, 1, "clamped at the start");
    }

    #[test]
    fn jumping_to_ends_works() {
        let mut picker = picker(vec![entry(1, "a", None), entry(2, "b", None), entry(3, "c", None)]);
        picker.select_last();
        assert_eq!(picker.selected().unwrap().process.pid, 3);
        picker.select_first();
        assert_eq!(picker.selected().unwrap().process.pid, 1);
    }

    #[test]
    fn an_empty_list_has_no_selection_and_does_not_panic() {
        let mut picker = picker(Vec::new());
        assert!(picker.selected().is_none());
        picker.move_by(1);
        picker.select_last();
        assert!(picker.selected().is_none());
    }

    #[test]
    fn skipped_sockets_are_described_for_the_user() {
        let discovery = Discovery { stale: 81, foreign: 2, ..Default::default() };
        let picker = Picker::new(discovery, Vec::new());
        let text = picker.skipped.describe();
        assert!(text.starts_with("83 sockets skipped"));
        assert!(text.contains("81 stale"));
        assert!(text.contains("2 other users"));
    }
}
