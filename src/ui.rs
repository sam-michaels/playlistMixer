use crate::{create_apple_playlist, form_error, in_dates, match_to_apple, order, pick, read_favorites, spotify, Result, Track};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

enum Screen {
    Menu,
    Form,
    Results,
    Done(String),
}

#[derive(Debug, PartialEq)]
enum Action {
    None,
    Generate,
    Create,
}

struct App {
    screen: Screen,
    menu_sel: usize, // 0 New playlist, 1 Quit
    from: String,
    to: String,
    vibe: String,
    focus: usize, // Form: 0 from, 1 to, 2 vibe, 3 [Generate]
    error: Option<String>,
    status: Option<String>,
    tracks: Vec<Track>,
    model: String,
    picks: Vec<Track>,
    checked: Vec<bool>,
    cursor: usize,
    list: ListState,
    name: String,
    name_focus: bool,
    quit: bool,
    token: Option<String>,
    playing: Option<usize>, // index in picks of the previewed song
    paused: bool,           // our view of Spotify's state after p presses
    to_spotify: bool, // Results save target; default Apple Music
}

impl App {
    fn new(tracks: Vec<Track>, model: String, token: Option<String>) -> Self {
        App {
            screen: Screen::Menu,
            menu_sel: 0,
            from: String::new(),
            to: String::new(),
            vibe: String::new(),
            focus: 0,
            error: None,
            status: None,
            tracks,
            model,
            picks: vec![],
            checked: vec![],
            cursor: 0,
            list: ListState::default(),
            name: String::new(),
            name_focus: false,
            quit: false,
            token,
            playing: None,
            paused: false,
            to_spotify: true, // Spotify is the default save target (only offered with the Spotify source)
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return Action::None;
        }
        self.error = None;
        // An Enter typed while songs load arrives as Ctrl-J (newline); don't let it act as 'j'.
        let key = if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('j') {
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        } else {
            key
        };
        match self.screen {
            Screen::Menu => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.menu_sel = 0,
                KeyCode::Down | KeyCode::Char('j') => self.menu_sel = 1,
                KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                KeyCode::Enter if self.menu_sel == 0 => self.screen = Screen::Form,
                KeyCode::Enter => self.quit = true,
                _ => {}
            },
            Screen::Form => return self.form_key(key),
            Screen::Results => return self.results_key(key),
            Screen::Done(_) => self.screen = Screen::Menu,
        }
        Action::None
    }

    fn form_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Menu,
            KeyCode::Tab => self.focus = (self.focus + 1) % 4,
            KeyCode::BackTab => self.focus = (self.focus + 3) % 4,
            KeyCode::Enter => match self.focus {
                0 | 1 => self.focus += 1,
                2 => self.vibe.push('\n'),
                _ => return Action::Generate,
            },
            KeyCode::Backspace => {
                match self.focus {
                    0 => self.from.pop(),
                    1 => self.to.pop(),
                    2 => self.vibe.pop(),
                    _ => None,
                };
            }
            KeyCode::Char(c) => match self.focus {
                0 | 1 if (c.is_ascii_digit() || c == '/') => {
                    let f = if self.focus == 0 { &mut self.from } else { &mut self.to };
                    if f.len() < 10 {
                        f.push(c);
                    }
                }
                2 => self.vibe.push(c),
                _ => {}
            },
            _ => {}
        }
        Action::None
    }

    fn results_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.status = None; // cap notice belongs to this result set only
                self.screen = Screen::Form;
            }
            KeyCode::Tab | KeyCode::BackTab => self.name_focus = !self.name_focus,
            KeyCode::Left | KeyCode::Right if self.token.is_some() => {
                self.to_spotify = key.code == KeyCode::Right
            }
            // Creating is deliberate: only Enter in the name box creates; in the list Enter toggles.
            KeyCode::Enter if self.name_focus => return Action::Create,
            KeyCode::Up if !self.name_focus => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down if !self.name_focus => {
                self.cursor = (self.cursor + 1).min(self.picks.len().saturating_sub(1))
            }
            KeyCode::Char(' ') | KeyCode::Enter if !self.name_focus => {
                if let Some(c) = self.checked.get_mut(self.cursor) {
                    *c = !*c;
                }
            }
            KeyCode::Char('p') if !self.name_focus && self.token.is_some() && !self.picks.is_empty() => {
                if self.playing == Some(self.cursor) {
                    spotify::toggle_pause();
                    self.paused = !self.paused;
                } else {
                    spotify::play_preview(&self.picks[self.cursor].id);
                    self.playing = Some(self.cursor);
                    self.paused = false;
                }
            }
            KeyCode::Char('a') if !self.name_focus => {
                let all = self.checked.iter().all(|c| *c);
                self.checked.iter_mut().for_each(|c| *c = !all);
            }
            KeyCode::Backspace if self.name_focus => {
                self.name.pop();
            }
            KeyCode::Char(c) if self.name_focus => self.name.push(c),
            _ => {}
        }
        Action::None
    }

    fn busy(&mut self, term: &mut DefaultTerminal, msg: String) -> Result<()> {
        self.status = Some(msg);
        term.draw(|f| self.draw(f))?;
        Ok(())
    }

    fn generate(&mut self, term: &mut DefaultTerminal) -> Result<()> {
        if let Some(e) = form_error(&self.from, &self.to) {
            self.error = Some(e);
            return Ok(());
        }
        let any = self.from.is_empty();
        let pool = in_dates(&self.tracks, &self.from, &self.to);
        if pool.is_empty() {
            self.error = Some("No favorites between those dates".into());
            return Ok(());
        }
        let mut capped = false;
        let no_vibe = self.vibe.trim().is_empty();
        let res = (|| -> Result<Vec<Track>> {
            if no_vibe {
                return Ok(pool.clone()); // no vibe: list every liked song in range, skip the model
            }
            let n = pool.chunks(80).count();
            let mut ids = std::collections::HashSet::new();
            for (i, b) in pool.chunks(80).enumerate() {
                self.busy(term, format!("Picking songs… batch {}/{n}", i + 1))?;
                ids.extend(pick(b, &self.vibe, &self.model)?);
            }
            let picked: Vec<Track> = pool.iter().filter(|t| ids.contains(&t.id)).cloned().collect();
            if picked.is_empty() {
                return Ok(picked);
            }
            if picked.len() > 150 {
                capped = true;
                return Ok(picked);
            }
            self.busy(term, "Arranging by flow…".into())?;
            let ids = order(&picked, &self.vibe, &self.model)?;
            Ok(ids.iter().filter_map(|id| picked.iter().find(|t| &t.id == id).cloned()).collect())
        })();
        self.status = capped.then(|| "Too many picks to arrange by flow — kept in favourite order".into());
        match res {
            Err(e) => self.error = Some(e.to_string()),
            Ok(p) if p.is_empty() => self.error = Some("No songs matched that vibe".into()),
            Ok(p) => {
                self.checked = vec![true; p.len()];
                self.picks = p;
                self.cursor = 0;
                self.playing = None;
                self.name = if !any {
                    format!("{} - {}", self.from, self.to)
                } else if no_vibe {
                    "All liked songs".into()
                } else {
                    self.vibe.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(40).collect()
                };
                self.name_focus = false;
                self.screen = Screen::Results;
            }
        }
        Ok(())
    }

    fn create(&mut self, term: &mut DefaultTerminal) -> Result<()> {
        let ids: Vec<String> =
            self.picks.iter().zip(&self.checked).filter(|(_, c)| **c).map(|(t, _)| t.id.clone()).collect();
        if ids.is_empty() {
            self.error = Some("Nothing selected".into());
            return Ok(());
        }
        let name = self.name.trim().to_string();
        if name.is_empty() {
            self.error = Some("Enter a playlist name".into());
            return Ok(());
        }
        let r = match (&self.token, self.to_spotify) {
            (Some(_), true) => {
                self.busy(term, "Creating Spotify playlist…".into())?;
                // refresh first: the token from startup expires after an hour
                spotify::login().and_then(|tok| {
                    self.token = Some(tok.clone());
                    spotify::create_playlist(&tok, &name, &ids)
                        .map(|n| format!("Added {n} tracks to \"{name}\" on Spotify"))
                })
            }
            (Some(_), false) => {
                self.busy(term, "Matching to your Apple Music favourites…".into())?;
                self.save_apple(&name, &ids)
            }
            (None, _) => {
                self.busy(term, "Creating playlist…".into())?;
                create_apple_playlist(&name, &ids).map(|n| format!("Added {n} tracks to \"{name}\""))
            }
        };
        self.status = None;
        match r {
            Ok(m) => self.screen = Screen::Done(m),
            Err(e) => self.error = Some(e.to_string()),
        }
        Ok(())
    }

    // Spotify picks -> Apple Music favourites by title+artist, then create the playlist
    fn save_apple(&self, name: &str, ids: &[String]) -> Result<String> {
        let chosen: Vec<Track> = self.picks.iter().filter(|t| ids.contains(&t.id)).cloned().collect();
        let (apple_ids, missing) = match_to_apple(&chosen, &read_favorites()?);
        if apple_ids.is_empty() {
            return Err("None of these songs are in your Apple Music favourites".into());
        }
        let n = create_apple_playlist(name, &apple_ids)?;
        let mut m = format!("Added {n} of {} to \"{name}\" in Apple Music", chosen.len());
        if !missing.is_empty() {
            m += &format!("\nNot found in Apple Music: {}", missing.iter().take(5).cloned().collect::<Vec<_>>().join(", "));
            if missing.len() > 5 {
                m += &format!(" +{} more", missing.len() - 5);
            }
        }
        Ok(m)
    }

    fn draw(&mut self, f: &mut Frame) {
        let [head, body, msg, foot] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1), Constraint::Length(1)])
                .areas(f.area());
        let stats = if self.token.is_some() {
            format!("{} liked songs on Spotify · {}", self.tracks.len(), self.model)
        } else {
            format!(
                "{} favourites · {} with dates · {}",
                self.tracks.len(),
                self.tracks.iter().filter(|t| t.added.is_some()).count(),
                self.model
            )
        };
        // The menu draws its own big logo and stats, so the header is only shown on the other screens.
        if !matches!(self.screen, Screen::Menu) {
            f.render_widget(Paragraph::new(vec![wordmark(), format!(" {stats}").dark_gray().into()]), head);
        }
        if let Some(e) = &self.error {
            f.render_widget(Paragraph::new(e.as_str()).red(), msg);
        } else if let Some(s) = &self.status {
            f.render_widget(Paragraph::new(s.as_str()).yellow(), msg);
        }
        let hint = match self.screen {
            Screen::Menu => "↑↓ move · Enter select · q/Esc quit",
            Screen::Form => "Tab/Shift-Tab next · Enter confirm · Esc menu · Ctrl-C quit",
            Screen::Results if self.name_focus && self.token.is_some() => {
                "Type a name · ←→ save to · Enter CREATE playlist · Tab back to list · Esc back"
            }
            Screen::Results if self.name_focus => "Type a name · Enter CREATE playlist · Tab back to list · Esc back",
            Screen::Results if self.token.is_some() => {
                "↑↓ move · Space/Enter tick · p play/pause · a all/none · Tab → name & create · Esc back"
            }
            Screen::Results => "↑↓ move · Space/Enter tick · a all/none · Tab → name & create · Esc back",
            Screen::Done(_) => "any key → menu",
        };
        f.render_widget(Paragraph::new(hint).dark_gray(), foot);

        match &self.screen {
            Screen::Menu => {
                let item = |i: usize, s: &str| -> ratatui::text::Line {
                    if self.menu_sel == i {
                        format!("› {s}").fg(SPOTIFY_GREEN).bold().into()
                    } else {
                        format!("  {s}").into()
                    }
                };
                let mut lines: Vec<ratatui::text::Line> = NOTE
                    .iter()
                    .enumerate()
                    .map(|(i, l)| format!("{l:<NOTE_W$}").fg(green(i as f32 / (NOTE.len() - 1) as f32)).into())
                    .collect();
                lines.extend([
                    "".into(),
                    wordmark(),
                    stats.clone().dark_gray().into(),
                    "".into(),
                    item(0, "New playlist"),
                    item(1, "Quit        "),
                ]);
                f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), body);
            }
            Screen::Form => {
                let [dates, vibe, button] =
                    Layout::vertical([Constraint::Length(3), Constraint::Min(6), Constraint::Length(1)]).areas(body);
                let [a, b] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(dates);
                f.render_widget(field("From", &self.from, "DD/MM/YYYY (blank = any)", self.focus == 0), a);
                f.render_widget(field("To", &self.to, "DD/MM/YYYY", self.focus == 1), b);
                f.render_widget(
                    field(
                        "Vibe profile (optional)",
                        &self.vibe,
                        "Leave empty to list every liked song in the range. Or describe it, e.g. Start restless and moody in June, build to carefree road-trip energy in July, end nostalgic",
                        self.focus == 2,
                    ),
                    vibe,
                );
                let style = if self.focus == 3 {
                    Style::new().bg(Color::Cyan).fg(Color::Black)
                } else {
                    Style::new()
                };
                f.render_widget(Paragraph::new("[ Generate ]").style(style), button);
            }
            Screen::Results => {
                let [list, name, save] =
                    Layout::vertical([Constraint::Min(3), Constraint::Length(3), Constraint::Length(1)]).areas(body);
                let items: Vec<ListItem> = self
                    .picks
                    .iter()
                    .zip(&self.checked)
                    .enumerate()
                    .map(|(i, (t, c))| {
                        let d = t.added.as_deref().filter(|d| d.len() >= 10).map_or("?".into(), |d| format!("{}/{}", &d[8..10], &d[5..7]));
                        let mut spans = vec![ratatui::text::Span::raw(format!(
                            "[{}] {} — {}   {d}",
                            if *c { 'x' } else { ' ' },
                            t.name,
                            t.artist
                        ))];
                        if self.playing == Some(i) {
                            spans.push(if self.paused { " ‖ paused".dark_gray() } else { " ♪".fg(SPOTIFY_GREEN) });
                        }
                        ListItem::new(ratatui::text::Line::from(spans))
                    })
                    .collect();
                self.list.select(Some(self.cursor));
                let l = List::new(items)
                    .block(boxed(
                        format!("Picks ({} of {} selected)", self.checked.iter().filter(|c| **c).count(), self.picks.len()),
                        !self.name_focus,
                    ))
                    .highlight_symbol("› ")
                    .highlight_style(Style::new().add_modifier(Modifier::BOLD));
                f.render_stateful_widget(l, list, &mut self.list);
                f.render_widget(field("Playlist name", &self.name, "", self.name_focus), name);
                if self.token.is_some() {
                    // Brand colours: the chosen target is bracketed and bold, the other is dimmed.
                    let opt = |label: &str, color: Color, on: bool| {
                        if on {
                            format!("‹ {label} ›").fg(color).bold()
                        } else {
                            format!("  {label}  ").fg(color).add_modifier(Modifier::DIM)
                        }
                    };
                    f.render_widget(
                        Paragraph::new(ratatui::text::Line::from(vec![
                            "Save to:  ".into(),
                            opt("Apple Music", APPLE_RED, !self.to_spotify),
                            "  ".into(),
                            opt("Spotify", SPOTIFY_GREEN, self.to_spotify),
                        ])),
                        save,
                    );
                }
            }
            Screen::Done(m) => {
                let [_, mid, _] =
                    Layout::vertical([Constraint::Percentage(40), Constraint::Length(1), Constraint::Min(0)]).areas(body);
                f.render_widget(Paragraph::new(m.as_str()).alignment(Alignment::Center).green(), mid);
            }
        }
    }
}

const SPOTIFY_GREEN: Color = Color::Rgb(30, 215, 96);
const APPLE_RED: Color = Color::Rgb(250, 36, 60);

// Two beamed eighth notes; every line is padded to NOTE_W so centring keeps the shape.
const NOTE_W: usize = 26;
const NOTE: [&str; 9] = [
    "        ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄",
    "        ██████████████████",
    "        ██▀▀▀▀▀▀▀▀▀▀▀▀▀▀██",
    "        ██              ██",
    "        ██              ██",
    "        ██              ██",
    "  ▄▄▄▄▄▄██        ▄▄▄▄▄▄██",
    "██████████      ██████████",
    "▀████████▀      ▀████████▀",
];

/// Green that deepens from Spotify green (t = 0) to a darker forest green (t = 1).
fn green(t: f32) -> Color {
    let mix = |a: f32, b: f32| (a + (b - a) * t).round() as u8;
    Color::Rgb(mix(30.0, 18.0), mix(215.0, 120.0), mix(96.0, 58.0))
}

/// " ♫  p l a y l i s t   m i x e r" with a per-letter green gradient.
fn wordmark() -> ratatui::text::Line<'static> {
    let text: Vec<char> = " ♫  p l a y l i s t   m i x e r".chars().collect();
    let n = (text.len() - 1) as f32;
    let spans: Vec<ratatui::text::Span> =
        text.iter().enumerate().map(|(i, c)| c.to_string().fg(green(i as f32 / n)).bold()).collect();
    ratatui::text::Line::from(spans)
}

fn boxed<'a>(title: impl Into<ratatui::text::Line<'a>>, focused: bool) -> Block<'a> {
    let c = if focused { Color::Cyan } else { Color::DarkGray };
    Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(c)).title(title)
}

fn field<'a>(title: &'a str, text: &'a str, placeholder: &'a str, focused: bool) -> Paragraph<'a> {
    let mut lines = vec![];
    if text.is_empty() {
        let mut spans = vec![];
        if focused {
            spans.push("█".into());
        }
        spans.push(placeholder.dark_gray());
        lines.push(ratatui::text::Line::from(spans));
    } else {
        let s = if focused { format!("{text}█") } else { text.to_string() };
        lines.extend(s.split('\n').map(|l| ratatui::text::Line::from(l.to_string())));
    }
    Paragraph::new(lines).wrap(Wrap { trim: false }).block(boxed(title, focused))
}

pub fn run(tracks: Vec<Track>, model: String, token: Option<String>) -> Result<()> {
    let mut term = ratatui::init();
    let r = event_loop(&mut term, App::new(tracks, model, token));
    ratatui::restore();
    r
}

fn event_loop(term: &mut DefaultTerminal, mut app: App) -> Result<()> {
    while !app.quit {
        term.draw(|f| app.draw(f))?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match app.on_key(key) {
                Action::None => {}
                Action::Generate => app.generate(term)?,
                Action::Create => app.create(term)?,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn type_str(a: &mut App, s: &str) {
        for c in s.chars() {
            a.on_key(k(KeyCode::Char(c)));
        }
    }

    #[test]
    fn keys() {
        let mut a = App::new(vec![], "m".into(), None);
        a.on_key(k(KeyCode::Enter));
        assert!(matches!(a.screen, Screen::Form));
        type_str(&mut a, "01/06/2026");
        a.on_key(k(KeyCode::Tab));
        type_str(&mut a, "31/08/2026");
        a.on_key(k(KeyCode::Tab));
        type_str(&mut a, "chill");
        a.on_key(k(KeyCode::Tab));
        assert_eq!(a.on_key(k(KeyCode::Enter)), Action::Generate);
        assert_eq!((a.from.as_str(), a.to.as_str(), a.vibe.as_str()), ("01/06/2026", "31/08/2026", "chill"));
        a.focus = 0;
        type_str(&mut a, "x");
        assert_eq!(a.from, "01/06/2026");
    }

    #[test]
    fn results_ticking() {
        let t = |id: &str| Track {
            id: id.into(), name: "n".into(), artist: "a".into(), genre: "".into(),
            added: None, played: None,
        };
        let mut a = App::new(vec![], "m".into(), None);
        a.picks = vec![t("1"), t("2"), t("3")];
        a.checked = vec![true; 3];
        a.screen = Screen::Results;
        // Enter in the list unticks instead of creating
        assert_eq!(a.on_key(k(KeyCode::Enter)), Action::None);
        assert_eq!(a.checked, [false, true, true]);
        a.on_key(k(KeyCode::Down));
        a.on_key(k(KeyCode::Char(' ')));
        assert_eq!(a.checked, [false, false, true]);
        a.on_key(k(KeyCode::Char('a'))); // not all ticked -> tick all
        assert_eq!(a.checked, [true; 3]);
        a.on_key(k(KeyCode::Char('a'))); // all ticked -> untick all
        assert_eq!(a.checked, [false; 3]);
        // Tab to the name box, then Enter creates
        a.on_key(k(KeyCode::Tab));
        assert_eq!(a.on_key(k(KeyCode::Enter)), Action::Create);
    }

    #[test]
    fn preview_key() {
        let t = |id: &str| Track {
            id: id.into(), name: "n".into(), artist: "a".into(), genre: "".into(),
            added: None, played: None,
        };
        let mut a = App::new(vec![], "m".into(), Some("tok".into()));
        a.picks = vec![t("1"), t("2")];
        a.checked = vec![true; 2];
        a.screen = Screen::Results;
        a.on_key(k(KeyCode::Down));
        a.on_key(k(KeyCode::Char('p')));
        assert_eq!(a.playing, Some(1));
        a.on_key(k(KeyCode::Char('p'))); // same song: pause toggle, still the playing one
        assert_eq!((a.playing, a.paused), (Some(1), true));
        a.on_key(k(KeyCode::Char('p')));
        assert!(!a.paused);
        a.on_key(k(KeyCode::Tab)); // name box: p types
        a.on_key(k(KeyCode::Char('p')));
        assert_eq!(a.name, "p");
    }
}
