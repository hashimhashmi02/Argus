//! Turning replies into text a person reads at a glance.
//!
//! Pure functions only: no I/O, no pipe, no engine. Formatting decisions are
//! the part of a CLI most likely to be fiddled with, and they should be
//! testable without starting anything.

use std::time::Duration;

use argus_protocol::{AgentInfo, ScreenView, SessionInfo, Status};

/// `45s`, `4m12s`, `1h04m`. Seconds stop mattering once minutes are double
/// digits into an hour, and a column of `1h04m37s` is noise.
pub fn duration(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// How a status is written in a listing.
///
/// `NEEDS YOU` in capitals and nothing else: it is the one status that is a
/// call to action, and in a column of lowercase words it is the thing the eye
/// should land on. Plain text rather than colour, so it survives being piped to
/// a file or read in a terminal that strips escape codes.
pub fn status(info: &SessionInfo) -> String {
    match info.status {
        Status::NeedsYou => "NEEDS YOU".to_string(),
        Status::Working => "working".to_string(),
        Status::Idle => "idle".to_string(),
        // Lost is checked first: it is always reported as done, but "done"
        // would claim the session finished. It did not -- it was running when
        // the engine stopped, and went down with it.
        Status::Done if info.lost => "lost".to_string(),
        Status::Done => match info.exit_code {
            Some(code) => format!("done ({code})"),
            None => "done".to_string(),
        },
    }
}

/// Order sessions by what most needs attention.
///
/// Not the engine's order. `argus-cli list` exists to answer "which window
/// should I look at", so blocked sessions come first -- the one waiting longest
/// at the very top -- then working, then idle, then finished. Within working and
/// idle the longest-held comes first too, since that is the one most likely to
/// have something to show for itself. `--json` keeps the engine's order, for
/// scripts that want it.
pub fn by_urgency(sessions: &mut [SessionInfo]) {
    fn rank(status: Status) -> u8 {
        match status {
            Status::NeedsYou => 0,
            Status::Working => 1,
            Status::Idle => 2,
            Status::Done => 3,
        }
    }
    // Stable, so sessions with equal rank and age keep creation order.
    sessions.sort_by(|a, b| {
        rank(a.status)
            .cmp(&rank(b.status))
            .then(b.status_for_ms.cmp(&a.status_for_ms))
    });
}

/// The `list` table.
pub fn session_table(sessions: &[SessionInfo]) -> String {
    if sessions.is_empty() {
        return "no sessions -- start one with `argus-cli spawn <agent>`\n".to_string();
    }

    // Only when some session has a worktree, so a table without any reads
    // exactly as it always did.
    let branches = sessions.iter().any(|s| s.branch.is_some());
    let mut header: Vec<String> = ["ID", "AGENT", "STATUS", "FOR", "PID", "RULE"]
        .map(String::from)
        .to_vec();
    if branches {
        header.insert(2, "BRANCH".into());
    }
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|s| {
            let mut row = vec![
                s.id.clone(),
                s.agent.clone(),
                // A mark rather than a column: only finished sessions can be
                // resumed, so a column would be blank for every live one.
                if s.resumable {
                    format!("{}*", status(s))
                } else {
                    status(s)
                },
                duration(Duration::from_millis(s.status_for_ms)),
                s.pid.map_or_else(|| "-".to_string(), |p| p.to_string()),
                s.rule.clone().unwrap_or_else(|| "-".to_string()),
            ];
            if branches {
                row.insert(2, s.branch.clone().unwrap_or_else(|| "-".to_string()));
            }
            row
        })
        .collect();

    // Column widths from the widest cell, measured in characters rather than
    // bytes so a non-ASCII agent name does not skew the alignment.
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }

    let mut out = String::new();
    for row in std::iter::once(&header).chain(&rows) {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, &width)| format!("{cell:<width$}"))
            .collect();
        // The last column is not padded, so lines carry no trailing spaces.
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    if sessions.iter().any(|s| s.resumable) {
        out.push_str("\n* can be resumed with `argus-cli resume <id>`\n");
    }
    out
}

/// The `agents` listing.
pub fn agent_table(agents: &[AgentInfo]) -> String {
    let width = agents
        .iter()
        .map(|a| a.id.chars().count())
        .chain(std::iter::once("AGENT".len()))
        .max()
        .unwrap_or(0);
    let mut out = format!("{:<width$}  NAME\n", "AGENT");
    for agent in agents {
        out.push_str(&format!("{:<width$}  {}\n", agent.id, agent.display_name));
    }
    out
}

/// One line describing a session, used as a header and after `spawn`.
pub fn session_line(s: &SessionInfo) -> String {
    let mut line = format!(
        "{}  {}  {} for {}",
        s.id,
        s.agent,
        status(s),
        duration(Duration::from_millis(s.status_for_ms))
    );
    if let Some(rule) = &s.rule {
        line.push_str(&format!("  (rule: {rule})"));
    }
    if let (Some(branch), Some(cwd)) = (&s.branch, &s.cwd) {
        line.push_str(&format!("  on {branch} in {cwd}"));
    }
    line
}

/// The `screen` output: a header, then the screen as the user would see it.
pub fn screen(view: &ScreenView) -> String {
    let mut out = format!(
        "-- {}  [{}x{}, cursor {},{}] --\n",
        session_line(&view.session),
        view.cols,
        view.rows,
        view.cursor_row,
        view.cursor_col
    );

    // Trailing blank rows are dropped: a fresh terminal is a few lines of
    // content above a screenful of nothing, and printing the nothing pushes the
    // content off the top of the reader's terminal.
    let used = view
        .lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |last| last + 1);
    for line in &view.lines[..used] {
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, status: Status, for_secs: u64) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            agent: "shell".into(),
            status,
            since_ms: 0,
            status_for_ms: for_secs * 1000,
            rule: None,
            pid: Some(100),
            exit_code: None,
            lost: false,
            resumable: false,
            cwd: None,
            branch: None,
        }
    }

    #[test]
    fn durations_get_coarser_as_they_grow() {
        assert_eq!(duration(Duration::from_secs(0)), "0s");
        assert_eq!(duration(Duration::from_secs(45)), "45s");
        assert_eq!(duration(Duration::from_secs(60)), "1m00s");
        assert_eq!(duration(Duration::from_secs(252)), "4m12s");
        assert_eq!(duration(Duration::from_secs(3599)), "59m59s");
        assert_eq!(duration(Duration::from_secs(3600)), "1h00m");
        assert_eq!(duration(Duration::from_secs(3877)), "1h04m");
    }

    #[test]
    fn needs_you_is_the_one_status_that_shouts() {
        assert_eq!(status(&info("a", Status::NeedsYou, 0)), "NEEDS YOU");
        assert_eq!(status(&info("a", Status::Idle, 0)), "idle");

        let mut done = info("a", Status::Done, 0);
        done.exit_code = Some(1);
        assert_eq!(status(&done), "done (1)");

        let mut lost = info("a", Status::Done, 0);
        lost.lost = true;
        assert_eq!(status(&lost), "lost", "a lost session did not finish");
    }

    #[test]
    fn blocked_sessions_sort_first_longest_waiting_on_top() {
        let mut sessions = vec![
            info("idle-old", Status::Idle, 900),
            info("blocked-recent", Status::NeedsYou, 5),
            info("done", Status::Done, 9999),
            info("working", Status::Working, 30),
            info("blocked-long", Status::NeedsYou, 240),
        ];
        by_urgency(&mut sessions);

        let order: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            order,
            [
                "blocked-long",
                "blocked-recent",
                "working",
                "idle-old",
                "done"
            ]
        );
    }

    #[test]
    fn the_table_is_aligned_and_has_no_trailing_spaces() {
        let mut a = info("3fa1c2d4", Status::NeedsYou, 252);
        a.agent = "claude-code".into();
        a.rule = Some("permission-prompt".into());
        let b = info("9b0e1172", Status::Idle, 12);

        let table = session_table(&[a, b]);
        let lines: Vec<&str> = table.lines().collect();

        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("ID        AGENT        STATUS"));
        assert!(lines[1].contains("NEEDS YOU") && lines[1].contains("4m12s"));
        // Every column starts at the same offset on every row.
        let status_col = lines[0].find("STATUS").unwrap();
        assert_eq!(&lines[1][status_col..status_col + 9], "NEEDS YOU");
        assert_eq!(&lines[2][status_col..status_col + 4], "idle");
        assert!(lines.iter().all(|line| !line.ends_with(' ')));
    }

    #[test]
    fn resumable_sessions_are_marked_with_a_footnote_saying_how() {
        let mut lost = info("3fa1c2d4", Status::Done, 4);
        lost.lost = true;
        lost.resumable = true;
        let live = info("9b0e1172", Status::Idle, 12);

        let table = session_table(&[live.clone(), lost]);
        assert!(table.contains("lost*"), "{table}");
        assert!(table.contains("argus-cli resume <id>"), "{table}");

        let plain = session_table(&[live]);
        assert!(!plain.contains('*') && !plain.contains("resume"), "{plain}");
    }

    #[test]
    fn a_branch_column_appears_only_when_a_session_has_a_worktree() {
        let plain = info("9b0e1172", Status::Idle, 12);
        assert!(!session_table(std::slice::from_ref(&plain)).contains("BRANCH"));

        let mut on_branch = info("3fa1c2d4", Status::Working, 3);
        on_branch.branch = Some("feature/login".into());
        let table = session_table(&[on_branch, plain]);
        let lines: Vec<&str> = table.lines().collect();
        let col = lines[0].find("BRANCH").unwrap();
        assert_eq!(&lines[1][col..col + 13], "feature/login");
        assert_eq!(&lines[2][col..col + 1], "-");
    }

    #[test]
    fn an_empty_list_says_how_to_start() {
        assert!(session_table(&[]).contains("argus-cli spawn"));
    }

    #[test]
    fn a_screen_drops_its_trailing_blank_rows() {
        let view = ScreenView {
            session: info("3fa1c2d4", Status::Idle, 3),
            cols: 80,
            rows: 6,
            cursor_row: 1,
            cursor_col: 17,
            lines: vec![
                "Microsoft Windows".into(),
                "C:\\Users\\Hashim>".into(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ],
        };

        let text = screen(&view);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "header + two content lines, got {lines:?}");
        assert!(lines[0].contains("80x6"));
        assert_eq!(lines[2], "C:\\Users\\Hashim>");
    }
}
