use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{FixedOffset, Utc};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode, size,
};
use tokio::time::sleep;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::naver::{Game, GameState, Lineup, NaverClient, Player, TextRelayData};

const GUTTER: usize = 2;
const TEAM_COL: usize = 7;
const RESET: &str = "\x1b[0m";

#[derive(Clone, Copy, Debug)]
pub enum Competition {
    Kbo,
    AsianGames,
}

impl Competition {
    fn category_id(self) -> &'static str {
        match self {
            Self::Kbo => "kbo",
            Self::AsianGames => "agbaseball",
        }
    }

    fn header(self) -> &'static str {
        match self {
            Self::Kbo => "KBO LIVE",
            Self::AsianGames => "ASIAN GAMES BASEBALL",
        }
    }

    fn panel_label(self) -> &'static str {
        match self {
            Self::Kbo => "KBO",
            Self::AsianGames => "AG",
        }
    }

    fn empty_message(self) -> &'static str {
        match self {
            Self::Kbo => "오늘 KBO 경기가 없습니다.",
            Self::AsianGames => "오늘 아시안게임 야구 경기가 없습니다.",
        }
    }
}

pub async fn run_live(
    client: &NaverClient,
    competition: Competition,
    date: &str,
    interval: Duration,
    once: bool,
) -> Result<()> {
    let screen = ScreenGuard::enter()?;

    loop {
        let cards = fetch_cards(client, competition, date).await?;
        render(competition, date, interval, &cards)?;

        if once {
            break;
        }

        match wait_for_tick(interval, screen.interactive()).await? {
            LiveAction::Refresh => continue,
            LiveAction::Quit => break,
            LiveAction::Tick => continue,
        }
    }

    Ok(())
}

async fn fetch_cards(
    client: &NaverClient,
    competition: Competition,
    date: &str,
) -> Result<Vec<GameCard>> {
    let games = client
        .baseball_games(date, competition.category_id())
        .await?;
    let mut cards = Vec::with_capacity(games.len());

    for (index, game) in games.into_iter().enumerate() {
        let card = match client.game_polling(&game.game_id).await {
            Ok(polling) => GameCard::from_polling(index, polling.game, polling.text_relay_data),
            Err(error) => GameCard::from_error(index, game, error.to_string()),
        };
        cards.push(card);
    }

    Ok(cards)
}

enum LiveAction {
    Tick,
    Refresh,
    Quit,
}

async fn wait_for_tick(interval: Duration, interactive: bool) -> Result<LiveAction> {
    if !interactive {
        sleep(interval).await;
        return Ok(LiveAction::Tick);
    }

    let deadline = Instant::now() + interval;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(LiveAction::Tick);
        }

        let remaining = deadline.saturating_duration_since(now);
        let poll_for = remaining.min(Duration::from_millis(150));
        if event::poll(poll_for).context("키 입력 대기 실패")? {
            if let Event::Key(key) = event::read().context("키 입력 읽기 실패")? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(LiveAction::Quit),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(LiveAction::Quit);
                    }
                    KeyCode::Char('r') => return Ok(LiveAction::Refresh),
                    _ => {}
                }
            }
        }
    }
}

struct ScreenGuard {
    interactive: bool,
}

impl ScreenGuard {
    fn enter() -> Result<Self> {
        let interactive = io::stdout().is_terminal();
        if interactive {
            enable_raw_mode().context("raw mode 진입 실패")?;
            execute!(io::stdout(), EnterAlternateScreen, Hide).context("대체 화면 진입 실패")?;
        }
        Ok(Self { interactive })
    }

    fn interactive(&self) -> bool {
        self.interactive
    }
}

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        if self.interactive {
            let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
            let _ = disable_raw_mode();
        }
    }
}

#[derive(Clone, Debug)]
struct RelayEvent {
    seqno: u64,
    text: String,
    state: Option<GameState>,
}

#[derive(Debug)]
struct GameCard {
    index: usize,
    game: Game,
    relay: Option<TextRelayData>,
    latest_events: Vec<RelayEvent>,
    state: Option<GameState>,
    error: Option<String>,
}

impl GameCard {
    fn from_polling(index: usize, game: Game, relay: Option<TextRelayData>) -> Self {
        let latest_events = relay
            .as_ref()
            .map(collect_events)
            .unwrap_or_default()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();
        let state = latest_events
            .iter()
            .find_map(|event| event.state.clone())
            .or_else(|| relay.as_ref().and_then(latest_state));

        Self {
            index,
            game,
            relay,
            latest_events,
            state,
            error: None,
        }
    }

    fn from_error(index: usize, game: Game, error: String) -> Self {
        Self {
            index,
            game,
            relay: None,
            latest_events: Vec::new(),
            state: None,
            error: Some(error),
        }
    }

    fn score(&self) -> (i32, i32) {
        let away = self
            .state
            .as_ref()
            .and_then(|state| state.away_score.as_deref())
            .and_then(|score| score.parse::<i32>().ok())
            .or(self.game.away_team_score)
            .unwrap_or_default();
        let home = self
            .state
            .as_ref()
            .and_then(|state| state.home_score.as_deref())
            .and_then(|score| score.parse::<i32>().ok())
            .or(self.game.home_team_score)
            .unwrap_or_default();
        (away, home)
    }

    fn header_status(&self) -> String {
        if let Some(relay) = &self.relay {
            let inning = relay
                .inn
                .map(|inn| format!("{inn}회{}", half_label(relay.home_or_away.as_deref())))
                .unwrap_or_else(|| self.game.status_label().to_string());
            let outs = self
                .state
                .as_ref()
                .and_then(|state| state.out.as_deref())
                .unwrap_or("0");
            return format!("{inning} {outs}사");
        }

        if self.game.is_finished() {
            self.game
                .current_inning
                .clone()
                .unwrap_or_else(|| self.game.status_label().to_string())
        } else {
            format!(
                "{} {}",
                self.game.status_label(),
                self.game.starts_at_hhmm()
            )
        }
    }

    fn attack_side(&self) -> Option<Side> {
        match self.relay.as_ref()?.home_or_away.as_deref() {
            Some("0") => Some(Side::Away),
            Some("1") => Some(Side::Home),
            _ => None,
        }
    }

    fn bases(&self) -> Bases {
        let Some(state) = &self.state else {
            return Bases::default();
        };
        Bases {
            first: occupied(state.base1.as_deref()),
            second: occupied(state.base2.as_deref()),
            third: occupied(state.base3.as_deref()),
        }
    }

    fn balls(&self) -> usize {
        parse_count(self.state.as_ref().and_then(|state| state.ball.as_deref()))
    }

    fn strikes(&self) -> usize {
        parse_count(
            self.state
                .as_ref()
                .and_then(|state| state.strike.as_deref()),
        )
    }

    fn outs(&self) -> usize {
        parse_count(self.state.as_ref().and_then(|state| state.out.as_deref()))
    }

    fn current_batter(&self) -> String {
        let Some(relay) = &self.relay else {
            return "-".to_string();
        };
        let Some(pcode) = self
            .state
            .as_ref()
            .and_then(|state| state.batter.as_deref())
        else {
            return "-".to_string();
        };

        let hitter_lists = match self.attack_side() {
            Some(Side::Away) => [&relay.away_lineup, &relay.away_entry],
            Some(Side::Home) => [&relay.home_lineup, &relay.home_entry],
            None => [&None, &None],
        };

        find_player(pcode, hitter_lists)
            .map(player_label)
            .unwrap_or_else(|| "-".to_string())
    }

    fn current_pitcher(&self) -> String {
        let Some(relay) = &self.relay else {
            return format_starters(&self.game);
        };
        let Some(pcode) = self
            .state
            .as_ref()
            .and_then(|state| state.pitcher.as_deref())
        else {
            return current_pitcher_fallback(&self.game, self.attack_side());
        };

        let pitcher_lists = match self.attack_side() {
            Some(Side::Away) => [&relay.home_lineup, &relay.home_entry],
            Some(Side::Home) => [&relay.away_lineup, &relay.away_entry],
            None => [&None, &None],
        };

        find_player(pcode, pitcher_lists)
            .map(player_label)
            .unwrap_or_else(|| current_pitcher_fallback(&self.game, self.attack_side()))
    }

    fn inning_lines(&self) -> (Vec<String>, Vec<String>) {
        if let Some(relay) = &self.relay {
            if let Some(score) = &relay.inning_score {
                let max_inning = score
                    .home
                    .keys()
                    .chain(score.away.keys())
                    .filter_map(|key| key.parse::<usize>().ok())
                    .max()
                    .unwrap_or(1);
                let home = (1..=max_inning)
                    .map(|inn| {
                        score
                            .home
                            .get(&inn.to_string())
                            .cloned()
                            .unwrap_or("-".to_string())
                    })
                    .collect();
                let away = (1..=max_inning)
                    .map(|inn| {
                        score
                            .away
                            .get(&inn.to_string())
                            .cloned()
                            .unwrap_or("-".to_string())
                    })
                    .collect();
                return (away, home);
            }
        }

        let away = non_empty_scores(&self.game.away_team_score_by_inning);
        let home = non_empty_scores(&self.game.home_team_score_by_inning);
        if !away.is_empty() || !home.is_empty() {
            return (away, home);
        }

        (Vec::new(), Vec::new())
    }

    fn recent_plays(&self, max: usize) -> Vec<String> {
        if let Some(error) = &self.error {
            return vec![format!("API 오류: {}", single_line(error))];
        }

        let mut seen = HashSet::new();
        let mut plays = Vec::new();
        for event in &self.latest_events {
            let text = single_line(&event.text);
            if text.is_empty() || !seen.insert(text.clone()) {
                continue;
            }
            plays.push(text);
            if plays.len() >= max {
                break;
            }
        }

        if plays.is_empty() {
            plays.push(if self.game.is_finished() {
                "문자중계 없음".to_string()
            } else {
                "문자중계 대기 중".to_string()
            });
        }

        plays
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Side {
    Away,
    Home,
}

#[derive(Default)]
struct Bases {
    first: bool,
    second: bool,
    third: bool,
}

fn render(
    competition: Competition,
    date: &str,
    interval: Duration,
    cards: &[GameCard],
) -> Result<()> {
    let (terminal_width, height) = terminal_size();
    let width = terminal_width.saturating_sub(1).max(1);
    let columns = 3;
    let cell_width = (width.saturating_sub(GUTTER * (columns - 1)) / columns).max(1);
    let header_lines = 1;
    let row_gap = 1;
    let panel_height = ((height.saturating_sub(header_lines + row_gap)) / 2).clamp(18, 26);
    let now = kst_now();

    let mut out = String::new();
    if io::stdout().is_terminal() {
        execute!(io::stdout(), MoveTo(0, 0), Clear(ClearType::All)).context("화면 초기화 실패")?;
    } else {
        out.push_str("\x1b[2J\x1b[H");
    }

    out.push_str(&fit(
        &format!(
            "{} {}",
            bold(competition.header()),
            dim(&format!(
                "· {date} · {now} KST · refresh {}s · q:종료 r:새로고침",
                interval.as_secs(),
            ))
        ),
        width,
    ));
    out.push('\n');

    if cards.is_empty() {
        out.push_str(competition.empty_message());
        out.push('\n');
        write_screen(out)?;
        return Ok(());
    }

    let panels = cards
        .iter()
        .map(|card| render_panel(card, competition, cards.len(), cell_width, panel_height))
        .collect::<Vec<_>>();

    append_grid_row(&mut out, &panels, 0, 3, cell_width, panel_height);
    out.push('\n');
    append_grid_row(&mut out, &panels, 3, 2, cell_width, panel_height);

    write_screen(out)?;
    Ok(())
}

fn write_screen(out: String) -> Result<()> {
    if io::stdout().is_terminal() {
        print!("{}", out.replace('\n', "\r\n"));
    } else {
        print!("{out}");
    }
    io::stdout().flush().context("보드 출력 실패")?;
    Ok(())
}

fn append_grid_row(
    out: &mut String,
    panels: &[Vec<String>],
    start: usize,
    count: usize,
    cell_width: usize,
    panel_height: usize,
) {
    for line_index in 0..panel_height {
        for col in 0..3 {
            if col > 0 {
                out.push_str(&" ".repeat(GUTTER));
            }

            if col < count {
                let panel_index = start + col;
                if let Some(panel) = panels.get(panel_index) {
                    out.push_str(panel.get(line_index).map(String::as_str).unwrap_or(""));
                    continue;
                }
            }

            out.push_str(&" ".repeat(cell_width));
        }
        out.push('\n');
    }
}

fn render_panel(
    card: &GameCard,
    competition: Competition,
    game_count: usize,
    width: usize,
    panel_height: usize,
) -> Vec<String> {
    let inner = width.saturating_sub(2);
    let body_height = panel_height.saturating_sub(5);
    let title = format!("{} · {}", dim(competition.panel_label()), status_text(card));
    let footer = dim(&format!("q:종료  r:새로고침 · {}", kst_now()));
    let mut body = panel_body(card, inner.saturating_sub(2), body_height);
    body.resize(body_height, String::new());

    let mut lines = Vec::with_capacity(panel_height);
    lines.push(top_border(&title, inner));
    for line in body {
        lines.push(format!(
            "{} {} {}",
            dim("│"),
            fit(&line, inner.saturating_sub(2)),
            dim("│")
        ));
    }
    lines.push(mid_border(inner));
    lines.push(format!(
        "{} {} {}",
        dim("│"),
        fit(&footer, inner.saturating_sub(2)),
        dim("│")
    ));
    lines.push(bottom_border(inner));
    lines.push(fit(
        &format!(
            "{} {} {}",
            dim(&format!("[{}/{}]", card.index + 1, game_count)),
            card.game.away_team_name,
            dim(&format!("vs {}", card.game.home_team_name))
        ),
        width,
    ));
    lines.resize_with(panel_height, || " ".repeat(width));
    lines
}

fn status_text(card: &GameCard) -> String {
    let label = card.header_status();
    if card.game.is_started() {
        format!("{} {}", green("● LIVE"), dim(&label))
    } else if card.game.is_finished() {
        dim(&label)
    } else {
        cyan(&label)
    }
}

fn panel_body(card: &GameCard, width: usize, height: usize) -> Vec<String> {
    let (away_score, home_score) = card.score();
    let attack = card.attack_side();
    let mut body = Vec::new();

    body.push(score_row(
        &card.game.away_team_name,
        away_score,
        attack == Some(Side::Away),
    ));
    body.push(score_row(
        &card.game.home_team_name,
        home_score,
        attack == Some(Side::Home),
    ));
    body.push(String::new());

    let diamond = diamond_lines(&card.bases());
    let count = count_lines(card.balls(), card.strikes(), card.outs());
    for index in 0..5 {
        let right = count.get(index).map(String::as_str).unwrap_or("");
        body.push(format!("{}  {}", diamond[index], right));
    }
    body.push(String::new());

    body.push(label_value("타자", &card.current_batter()));
    body.push(label_value("투수", &card.current_pitcher()));
    body.push(String::new());

    for line in inning_table(card, width) {
        body.push(line);
    }
    body.push(String::new());
    body.push(dim("─ 최근 플레이 ─"));

    let remaining = height.saturating_sub(body.len()).max(1);
    for play in card.recent_plays(remaining) {
        body.push(format!("  {} {}", dim("▸"), play));
    }

    body.truncate(height);
    body
}

fn score_row(team: &str, score: i32, attacking: bool) -> String {
    let badge = pad_right_visual(&team_badge(team), 8);
    let score = pad_left_visual(&team_fg(team, &bold(&score.to_string())), 3);
    let marker = if attacking {
        format!("  {}", cyan("◀ 공격"))
    } else {
        String::new()
    };
    format!("  {badge}  {score}{marker}")
}

fn label_value(label: &str, value: &str) -> String {
    format!("  {}  {}", dim(&pad_right_visual(label, 4)), value)
}

fn diamond_lines(bases: &Bases) -> [String; 5] {
    [
        format!("       {}       ", base_dot(bases.second)),
        dim("     ╱   ╲     "),
        format!(
            "   {}       {}   ",
            base_dot(bases.third),
            base_dot(bases.first)
        ),
        dim("     ╲   ╱     "),
        dim("       ⌂       "),
    ]
}

fn count_lines(ball: usize, strike: usize, out: usize) -> [String; 5] {
    [
        String::new(),
        format!("B  {}", dots(ball, 3, DotColor::Green)),
        format!("S  {}", dots(strike, 2, DotColor::Yellow)),
        format!("O  {}", dots(out, 2, DotColor::Red)),
        String::new(),
    ]
}

fn inning_table(card: &GameCard, width: usize) -> Vec<String> {
    let (away, home) = card.inning_lines();
    if away.is_empty() && home.is_empty() {
        return vec![dim("  회"), dim("  스코어보드 대기 중")];
    }

    let max_innings_by_width = width
        .saturating_sub(TEAM_COL + 2)
        .saturating_div(3)
        .clamp(1, 12);
    let innings = away.len().max(home.len()).min(max_innings_by_width);
    let header = (1..=innings)
        .map(|inning| format!("{inning:>2}"))
        .collect::<Vec<_>>()
        .join(" ");
    let away_scores = score_cells(&away, innings);
    let home_scores = score_cells(&home, innings);

    vec![
        format!(
            "  {} {}",
            dim(&pad_right_visual("회", TEAM_COL)),
            dim(&header)
        ),
        format!(
            "  {} {}",
            team_fg(
                &card.game.away_team_name,
                &pad_right_visual(&card.game.away_team_name, TEAM_COL)
            ),
            away_scores
        ),
        format!(
            "  {} {}",
            team_fg(
                &card.game.home_team_name,
                &pad_right_visual(&card.game.home_team_name, TEAM_COL)
            ),
            home_scores
        ),
    ]
}

fn score_cells(scores: &[String], innings: usize) -> String {
    (0..innings)
        .map(|index| scores.get(index).cloned().unwrap_or("-".to_string()))
        .map(|score| {
            if score == "-" {
                dim(" -")
            } else {
                format!("{score:>2}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn top_border(title: &str, inner: usize) -> String {
    let raw_title = format!(" {title} ");
    let title = if visual_width(&raw_title) > inner.saturating_sub(1) {
        fit(&raw_title, inner.saturating_sub(1))
            .trim_end()
            .to_string()
    } else {
        raw_title
    };
    let fill = inner.saturating_sub(visual_width(&title) + 1);
    format!(
        "{}{}{}{}",
        dim("┌─"),
        title,
        dim(&"─".repeat(fill)),
        dim("┐")
    )
}

fn mid_border(inner: usize) -> String {
    dim(&format!("├{}┤", "─".repeat(inner)))
}

fn bottom_border(inner: usize) -> String {
    dim(&format!("└{}┘", "─".repeat(inner)))
}

fn fit(text: &str, width: usize) -> String {
    let text = display_line(text);
    let current = visual_width(&text);
    if current <= width {
        return format!("{}{}", text, " ".repeat(width - current));
    }

    let mut output = String::new();
    let mut used = 0;
    let limit = width.saturating_sub(1);
    let mut chars = text.chars().peekable();
    let mut had_ansi = false;

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            had_ansi = true;
            output.push(ch);
            for next in chars.by_ref() {
                output.push(next);
                if next == 'm' {
                    break;
                }
            }
            continue;
        }

        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width > limit {
            break;
        }
        output.push(ch);
        used += ch_width;
    }

    if had_ansi {
        output.push_str(RESET);
    }
    output.push('…');

    let final_width = visual_width(&output);
    if final_width < width {
        output.push_str(&" ".repeat(width - final_width));
    }
    output
}

fn display_line(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\r' | '\n' | '\t' => ' ',
            _ => ch,
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn visual_width(text: &str) -> usize {
    UnicodeWidthStr::width(strip_ansi(text).as_str())
}

fn strip_ansi(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

fn pad_right_visual(text: &str, width: usize) -> String {
    let text = display_line(text);
    let current = visual_width(&text);
    if current >= width {
        return text;
    }

    format!("{}{}", text, " ".repeat(width - current))
}

fn pad_left_visual(text: &str, width: usize) -> String {
    let text = display_line(text);
    let current = visual_width(&text);
    if current >= width {
        return text;
    }

    format!("{}{}", " ".repeat(width - current), text)
}

fn style_enabled() -> bool {
    io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn sgr(code: &str, text: &str) -> String {
    if style_enabled() {
        format!("\x1b[{code}m{text}{RESET}")
    } else {
        text.to_string()
    }
}

fn bold(text: &str) -> String {
    sgr("1", text)
}

fn dim(text: &str) -> String {
    sgr("2", text)
}

fn cyan(text: &str) -> String {
    sgr("36", text)
}

fn green(text: &str) -> String {
    sgr("32", text)
}

fn yellow(text: &str) -> String {
    sgr("33", text)
}

fn red(text: &str) -> String {
    sgr("31", text)
}

fn team_badge(team: &str) -> String {
    let label = format!(" {} ", team_display(team));
    let Some((r, g, b)) = team_rgb(team) else {
        return bold(&label);
    };
    if !style_enabled() {
        return label;
    }

    let brightness = (u16::from(r) * 299 + u16::from(g) * 587 + u16::from(b) * 114) / 1000;
    let fg = if brightness > 128 {
        "0;0;0"
    } else {
        "255;255;255"
    };
    format!("\x1b[1m\x1b[48;2;{r};{g};{b}m\x1b[38;2;{fg}m{label}\x1b[49m\x1b[39m\x1b[22m")
}

fn team_fg(team: &str, text: &str) -> String {
    let Some((r, g, b)) = team_rgb(team).map(bright_rgb) else {
        return bold(text);
    };
    if !style_enabled() {
        return text.to_string();
    }
    format!("\x1b[38;2;{r};{g};{b}m{text}\x1b[39m")
}

fn team_display(team: &str) -> &str {
    match team {
        "KT" => "K T",
        "LG" => "L G",
        "NC" => "N C",
        _ => team,
    }
}

fn team_rgb(team: &str) -> Option<(u8, u8, u8)> {
    match team {
        "LG" => Some((195, 4, 82)),
        "두산" => Some((26, 23, 72)),
        "KIA" => Some((234, 0, 41)),
        "KT" => Some((0, 0, 0)),
        "삼성" => Some((7, 76, 161)),
        "한화" => Some((252, 78, 0)),
        "SSG" => Some((206, 14, 45)),
        "롯데" => Some((4, 30, 66)),
        "NC" => Some((49, 82, 136)),
        "키움" => Some((87, 5, 20)),
        "대한민국" => Some((0, 71, 160)),
        "일본" => Some((188, 0, 45)),
        "중국" => Some((222, 41, 16)),
        "차이니스 타이베이" => Some((0, 51, 160)),
        "홍콩" => Some((222, 41, 16)),
        "태국" => Some((36, 29, 112)),
        "필리핀" => Some((0, 56, 168)),
        "팔레스타인" => Some((0, 122, 61)),
        _ => None,
    }
}

fn bright_rgb((r, g, b): (u8, u8, u8)) -> (u8, u8, u8) {
    let max = r.max(g).max(b);
    if max == 0 {
        return (160, 160, 160);
    }
    if max >= 150 {
        return (r, g, b);
    }

    let factor = 150.0 / f32::from(max);
    (
        (f32::from(r) * factor).round().min(255.0) as u8,
        (f32::from(g) * factor).round().min(255.0) as u8,
        (f32::from(b) * factor).round().min(255.0) as u8,
    )
}

fn base_dot(occupied: bool) -> String {
    if occupied { yellow("◆") } else { dim("◇") }
}

enum DotColor {
    Green,
    Yellow,
    Red,
}

fn dots(filled: usize, total: usize, color: DotColor) -> String {
    let mut output = String::new();
    for index in 0..total {
        if index < filled {
            let dot = match color {
                DotColor::Green => green("●"),
                DotColor::Yellow => yellow("●"),
                DotColor::Red => red("●"),
            };
            output.push_str(&dot);
        } else {
            output.push_str(&dim("○"));
        }
    }
    output
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_events(data: &TextRelayData) -> Vec<RelayEvent> {
    let mut events = Vec::new();

    for group in &data.text_relays {
        for (fallback_index, option) in group.text_options.iter().enumerate() {
            let Some(text) = option.text.as_deref().map(str::trim).filter(|text| {
                !text.is_empty() && !text.chars().all(|ch| ch == '=' || ch == '-' || ch == ' ')
            }) else {
                continue;
            };

            let seqno = option
                .seqno
                .or_else(|| {
                    group
                        .no
                        .map(|no| no.saturating_mul(1_000) + fallback_index as u64)
                })
                .unwrap_or(fallback_index as u64);

            events.push(RelayEvent {
                seqno,
                text: text.to_string(),
                state: option.current_game_state.clone(),
            });
        }
    }

    events.sort_by_key(|event| event.seqno);
    events.dedup_by_key(|event| event.seqno);
    events
}

fn latest_state(relay: &TextRelayData) -> Option<GameState> {
    relay.current_game_state.clone().or_else(|| {
        relay
            .text_relays
            .iter()
            .flat_map(|group| group.text_options.iter())
            .filter_map(|option| option.current_game_state.clone())
            .last()
    })
}

fn find_player<'a>(pcode: &str, lineups: [&'a Option<Lineup>; 2]) -> Option<&'a Player> {
    lineups
        .into_iter()
        .filter_map(Option::as_ref)
        .flat_map(|lineup| lineup.batter.iter().chain(lineup.pitcher.iter()))
        .find(|player| player.pcode == pcode)
}

fn player_label(player: &Player) -> String {
    if let Some(avg) = player.today_hra.or(player.season_hra) {
        return format!(
            "{} {}",
            bold(&player.name),
            dim(&format!("AVG {}", format_avg(avg)))
        );
    }
    if let Some(era) = player
        .today_era
        .map(|era| format!("{era:.2}"))
        .or_else(|| player.season_era.clone())
    {
        return format!("{} {}", bold(&player.name), dim(&format!("ERA {era}")));
    }
    bold(&player.name)
}

fn format_avg(value: f64) -> String {
    let text = format!("{value:.3}");
    text.strip_prefix('0').unwrap_or(&text).to_string()
}

fn current_pitcher_fallback(game: &Game, attack: Option<Side>) -> String {
    match attack {
        Some(Side::Away) => game
            .home_current_pitcher_name
            .as_deref()
            .or(game.home_starter_name.as_deref())
            .map(bold)
            .unwrap_or_else(|| dim("-")),
        Some(Side::Home) => game
            .away_current_pitcher_name
            .as_deref()
            .or(game.away_starter_name.as_deref())
            .map(bold)
            .unwrap_or_else(|| dim("-")),
        None => format_starters(game),
    }
}

fn format_starters(game: &Game) -> String {
    match (
        game.away_starter_name.as_deref(),
        game.home_starter_name.as_deref(),
    ) {
        (Some(away), Some(home)) if !away.is_empty() || !home.is_empty() => {
            format!("{} {away} {} {home}", dim("선발"), dim("vs"))
        }
        _ => dim("-"),
    }
}

fn non_empty_scores(scores: &[String]) -> Vec<String> {
    scores
        .iter()
        .filter(|score| !score.is_empty())
        .cloned()
        .collect()
}

fn half_label(value: Option<&str>) -> &'static str {
    match value {
        Some("0") => "초",
        Some("1") => "말",
        _ => "",
    }
}

fn occupied(value: Option<&str>) -> bool {
    !matches!(value, None | Some("") | Some("0"))
}

fn parse_count(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}

fn terminal_size() -> (usize, usize) {
    if io::stdout().is_terminal() {
        if let Ok((width, height)) = size() {
            return (width as usize, height as usize);
        }
    }

    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(180);
    let height = std::env::var("LINES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(50);
    (width, height)
}

fn kst_now() -> String {
    let kst = FixedOffset::east_opt(9 * 60 * 60).expect("KST offset is valid");
    Utc::now()
        .with_timezone(&kst)
        .format("%H:%M:%S")
        .to_string()
}
