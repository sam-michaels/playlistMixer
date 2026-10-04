use clap::Parser;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;
mod spotify;
mod ui;

use std::process::Command;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "qwen2.5:7b")]
    model: String,
    /// spotify (liked songs) or apple (Music.app favourites)
    #[arg(long, default_value = "spotify", value_parser = ["spotify", "apple"])]
    source: String,
}

#[derive(Deserialize, Clone)]
#[allow(dead_code)]
struct Track {
    id: String,
    name: String,
    artist: String,
    album: String,
    genre: String,
    added: Option<String>,
    played: Option<String>, // ISO-8601 strings
}

const READ_JS: &str = r#"
const M = Application('Music');
const names = M.userPlaylists.name(); const i = names.findIndex(n=>/^Favou?rite Songs$/.test(n));
const t = (i>=0 ? M.userPlaylists[i] : M.libraryPlaylists[0]).tracks.whose({favorited: true});
const id=t.persistentID(), n=t.name(), a=t.artist(), al=t.album(), g=t.genre(), ad=t.dateAdded(), pl=t.playedDate();
const seen = new Set();
JSON.stringify(id.map((x,i)=>({id:x,name:n[i],artist:a[i],album:al[i],genre:g[i],
  added: ad[i]?ad[i].toISOString():null, played: pl[i]?pl[i].toISOString():null})).filter(o=>!seen.has(o.id)&&seen.add(o.id)))
"#;

const CREATE_JS: &str = r#"
function run(argv){
  const M=Application('Music'), lib=M.libraryPlaylists[0];
  const names=M.userPlaylists.name(), i=names.findIndex(n=>/^Favou?rite Songs$/.test(n));
  const srcs=(i>=0?[M.userPlaylists[i],lib]:[lib]);
  const p=M.UserPlaylist({name: argv[0]}).make();
  argv.slice(1).forEach(id=>{
    for(const s of srcs){ const t=s.tracks.whose({persistentID:id}); if(t.length){ M.duplicate(t[0],{to:p}); break; } }
  });
  return p.tracks.length;
}
"#;

const SYSTEM: &str = r#"You choose songs for a playlist. The user gives a vibe profile: the moods of a period of their life and how the playlist should flow. From the numbered song list, return JSON {"ids": [...]} with the numbers of the songs whose mood, sound or lyrics fit the description. Use what you know about each song. Return only numbers from the list."#;

fn osascript(js: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", js])
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "osascript failed: {}\nCheck the macOS Automation permission (System Settings → Privacy & Security → Automation).",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn read_favorites() -> Result<Vec<Track>> {
    Ok(serde_json::from_str(&osascript(READ_JS, &[])?)?)
}

// ponytail: UTC dates, can be off by a few hours at the range edges; parse to local time if that matters
fn in_range(t: &Track, from: &str, to: &str) -> bool {
    [&t.added, &t.played]
        .into_iter()
        .flatten()
        .filter_map(|d| d.get(..10))
        .any(|d| d >= from && d <= to)
}

fn keep_known(ids: Vec<String>, batch: &[Track]) -> Vec<String> {
    let known: HashSet<&str> = batch.iter().map(|t| t.id.as_str()).collect();
    let mut seen = HashSet::new();
    ids.into_iter()
        .filter(|i| known.contains(i.as_str()) && seen.insert(i.clone()))
        .collect()
}

fn song_lines(batch: &[Track]) -> String {
    let mut s = String::new();
    for (i, t) in batch.iter().enumerate() {
        let added = t.added.as_deref().and_then(|d| d.get(..10)).unwrap_or("?");
        s += &format!("\n{} | {} | {} | {} | added {}", i + 1, t.name, t.artist, t.genre, added);
    }
    s
}

// numbers are 1-based positions in the list sent; accepts numbers or numeric strings, ignores the rest
fn ids_from_numbers(nums: Vec<serde_json::Value>, batch: &[Track]) -> Vec<String> {
    nums.iter()
        .filter_map(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
        .filter_map(|n| batch.get((n as usize).checked_sub(1)?))
        .map(|t| t.id.clone())
        .collect()
}

fn norm(s: &str) -> String {
    let cut = [" (", " [", " - "].iter().filter_map(|p| s.find(p)).min().unwrap_or(s.len());
    s[..cut].to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

fn key(t: &Track) -> String {
    let first = t.artist.split([',', '&']).next().unwrap_or("");
    format!("{}|{}", norm(&t.name), norm(first))
}

fn match_to_apple(picks: &[Track], apple: &[Track]) -> (Vec<String>, Vec<String>) {
    let mut by_key = std::collections::HashMap::new();
    for a in apple.iter().filter(|a| !norm(&a.name).is_empty()) {
        by_key.entry(key(a)).or_insert(&a.id);
    }
    let (mut ids, mut missing) = (vec![], vec![]);
    for p in picks {
        match Some(p).filter(|p| !norm(&p.name).is_empty()).and_then(|p| by_key.get(&key(p))) {
            Some(id) => ids.push((*id).clone()),
            None => missing.push(format!("{} — {}", p.name, p.artist)),
        }
    }
    (ids, missing)
}

fn pick(batch: &[Track], desc: &str, model: &str) -> Result<Vec<String>> {
    let user = format!("{desc}\n{}", song_lines(batch));
    let body = json!({
        "model": model, "stream": false, "format": "json",
        "options": {"temperature": 0.2, "num_ctx": 16384},
        "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": user}]
    });
    let resp: serde_json::Value = ureq::post("http://localhost:11434/api/chat")
        .send_json(body)
        .map_err(|e| format!("Ollama request failed ({e}); start Ollama and run: ollama pull {model}"))?
        .into_json()?;
    let content = resp["message"]["content"].as_str().ok_or("no message.content in Ollama reply")?;
    #[derive(Deserialize)]
    struct Ids {
        ids: Vec<serde_json::Value>,
    }
    let ids: Ids = serde_json::from_str(content)?;
    Ok(keep_known(ids_from_numbers(ids.ids, batch), batch))
}

fn valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 2 || i == 5 { *c == b'/' } else { c.is_ascii_digit() })
        && matches!(s[..2].parse::<u8>(), Ok(1..=31))
        && matches!(s[3..5].parse::<u8>(), Ok(1..=12))
}

// DD/MM/YYYY -> YYYY-MM-DD; None unless the input is a valid date (never panics on blank input)
fn iso(s: &str) -> Option<String> {
    valid_date(s).then(|| format!("{}-{}-{}", &s[6..], &s[3..5], &s[..2]))
}

/// Tracks in the DD/MM/YYYY range, or every track when both dates are blank.
fn in_dates(tracks: &[Track], from: &str, to: &str) -> Vec<Track> {
    match (iso(from), iso(to)) {
        (Some(f), Some(t)) => tracks.iter().filter(|x| in_range(x, &f, &t)).cloned().collect(),
        _ => tracks.to_vec(), // form_error already rejected anything but both-blank
    }
}

// The vibe is optional: without one, every liked song in the range is listed.
fn form_error(from: &str, to: &str) -> Option<String> {
    let e = if from.is_empty() && to.is_empty() {
        return None;
    } else if from.is_empty() || to.is_empty() {
        "Fill both dates, or leave both empty for any date"
    } else if !valid_date(from) {
        "From must be DD/MM/YYYY"
    } else if !valid_date(to) {
        "To must be DD/MM/YYYY"
    } else if iso(from) > iso(to) {
        "From is after To"
    } else {
        return None;
    };
    Some(e.into())
}

fn fill_order(ids: Vec<String>, picks: &[Track]) -> Vec<String> {
    let mut out = keep_known(ids, picks);
    let have: HashSet<String> = out.iter().cloned().collect();
    out.extend(picks.iter().filter(|t| !have.contains(&t.id)).map(|t| t.id.clone()));
    out
}

// ponytail: one ordering call for all picks; skipped above 150 picks (keeps pick order), batch it if that matters
fn order(picks: &[Track], vibe: &str, model: &str) -> Result<Vec<String>> {
    let user = format!("{vibe}\n{}", song_lines(picks));
    let sys = r#"Arrange these songs into a playlist whose sequence follows the vibe profile's flow from start to end. Return JSON {"ids": [...]} containing the number of every song exactly once, in playing order."#;
    let body = json!({
        "model": model, "stream": false, "format": "json",
        "options": {"temperature": 0.2, "num_ctx": 16384},
        "messages": [{"role": "system", "content": sys}, {"role": "user", "content": user}]
    });
    let resp: serde_json::Value = ureq::post("http://localhost:11434/api/chat")
        .send_json(body)
        .map_err(|e| format!("Ollama request failed ({e}); start Ollama and run: ollama pull {model}"))?
        .into_json()?;
    let content = resp["message"]["content"].as_str().ok_or("no message.content in Ollama reply")?;
    #[derive(Deserialize)]
    struct Ids {
        ids: Vec<serde_json::Value>,
    }
    let ids: Ids = serde_json::from_str(content)?;
    Ok(fill_order(ids_from_numbers(ids.ids, picks), picks))
}

fn create_apple_playlist(name: &str, ids: &[String]) -> Result<usize> {
    let mut args = vec![name];
    args.extend(ids.iter().map(String::as_str));
    Ok(osascript(CREATE_JS, &args)?.parse()?)
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.source == "apple" {
        println!("Reading your Music library…");
        let tracks = read_favorites()?;
        return ui::run(tracks, args.model, None);
    }
    println!("Logging in to Spotify…");
    let token = spotify::login()?;
    println!("Loading your liked songs…");
    let tracks = spotify::read_likes(&token)?;
    ui::run(tracks, args.model, Some(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(id: &str, added: Option<&str>, played: Option<&str>) -> Track {
        Track {
            id: id.into(), name: "n".into(), artist: "a".into(), album: "al".into(), genre: "g".into(),
            added: added.map(Into::into), played: played.map(Into::into),
        }
    }

    #[test]
    fn range() {
        let (f, t) = ("2026-06-01", "2026-08-31");
        assert!(in_range(&tr("1", Some("2026-07-04T10:00:00.000Z"), None), f, t));
        assert!(in_range(&tr("1", Some("2020-01-01T00:00:00.000Z"), Some("2026-06-01T00:00:00.000Z")), f, t));
        assert!(!in_range(&tr("1", Some("2025-07-04T00:00:00.000Z"), Some("2027-01-01T00:00:00.000Z")), f, t));
        assert!(!in_range(&tr("1", None, None), f, t));
    }

    #[test]
    fn known() {
        let b = [tr("a", None, None), tr("b", None, None)];
        let ids = ["a", "x", "a", "b"].map(String::from).to_vec();
        assert_eq!(keep_known(ids, &b), vec!["a", "b"]);
    }

    #[test]
    fn dates() {
        assert!(valid_date("01/06/2026"));
        for bad in ["2026-04-30", "32/01/2026", "01/13/2026", "1/4/2026", ""] {
            assert!(!valid_date(bad), "{bad}");
        }
        assert_eq!(iso("30/04/2026").as_deref(), Some("2026-04-30"));
        assert_eq!(iso(""), None);
        // regression: blank dates used to panic in iso(); they must mean "every track"
        let all = [tr("a", Some("2024-07-01T00:00:00Z"), None), tr("b", Some("2020-01-01T00:00:00Z"), None)];
        assert_eq!(in_dates(&all, "", "").len(), 2);
        assert_eq!(in_dates(&all, "01/06/2024", "31/08/2024").len(), 1);
    }

    #[test]
    fn form() {
        let e = form_error;
        assert_eq!(e("x", "01/02/2026").unwrap(), "From must be DD/MM/YYYY");
        assert_eq!(e("01/01/2026", "x").unwrap(), "To must be DD/MM/YYYY");
        assert_eq!(e("02/01/2026", "01/01/2026").unwrap(), "From is after To");
        assert!(e("01/01/2026", "02/01/2026").is_none());
        assert!(e("", "").is_none());
        let msg = "Fill both dates, or leave both empty for any date";
        assert_eq!(e("", "01/02/2026").unwrap(), msg);
        assert_eq!(e("01/01/2026", "").unwrap(), msg);
    }

    #[test]
    fn numbers() {
        let b = [tr("a", None, None), tr("b", None, None), tr("c", None, None)];
        let nums = vec![json!(1), json!("2"), json!(99), json!("x")];
        assert_eq!(ids_from_numbers(nums, &b), vec!["a", "b"]);
    }

    #[test]
    fn apple_match() {
        let t = |id: &str, n: &str, a: &str| Track {
            id: id.into(), name: n.into(), artist: a.into(), album: String::new(), genre: String::new(),
            added: None, played: None,
        };
        let apple = [t("A1", "Mr. Brightside - 2004 Remaster", "The Killers"), t("A2", "Song", "A")];
        let picks = [t("s1", "Mr. Brightside", "The Killers"), t("s2", "Song (feat. X)", "A, B"), t("s3", "Nope", "Z"), t("s4", "!!!", "Q")];
        let (ids, missing) = match_to_apple(&picks, &apple);
        assert_eq!(ids, vec!["A1", "A2"]);
        assert_eq!(missing, vec!["Nope — Z", "!!! — Q"]);
    }

    #[test]
    fn fill() {
        let p = [tr("a", None, None), tr("b", None, None), tr("c", None, None)];
        let ids = ["c", "x", "c", "a"].map(String::from).to_vec();
        assert_eq!(fill_order(ids, &p), vec!["c", "a", "b"]);
    }
}
