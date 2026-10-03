use clap::Parser;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;
use std::io::{self, Write};
use std::process::Command;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

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

#[derive(Parser)]
struct Args {
    /// Start date, YYYY-MM-DD
    #[arg(long)]
    from: String,
    /// End date, YYYY-MM-DD
    #[arg(long)]
    to: String,
    #[arg(long, default_value = "qwen2.5:7b")]
    model: String,
    /// How the period felt
    description: String,
}

const READ_JS: &str = r#"
const M = Application('Music');
const t = M.libraryPlaylists[0].tracks.whose({favorited: true});
const id=t.persistentID(), n=t.name(), a=t.artist(), al=t.album(), g=t.genre(), ad=t.dateAdded(), pl=t.playedDate();
JSON.stringify(id.map((x,i)=>({id:x,name:n[i],artist:a[i],album:al[i],genre:g[i],
  added: ad[i]?ad[i].toISOString():null, played: pl[i]?pl[i].toISOString():null})))
"#;

const CREATE_JS: &str = r#"
function run(argv){
  const M=Application('Music'), lib=M.libraryPlaylists[0];
  const p=M.UserPlaylist({name: argv[0]}).make();
  argv.slice(1).forEach(id=>{ const t=lib.tracks.whose({persistentID:id}); if(t.length) M.duplicate(t[0],{to:p}); });
  return p.tracks.length;
}
"#;

const SYSTEM: &str = r#"You choose songs for a playlist. The user describes how a period of their life felt. From the numbered song list, return JSON {"ids": [...]} with the ids of songs whose mood, sound or lyrics fit the description. Use what you know about each song. Return only ids from the list."#;

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

fn pick(batch: &[Track], desc: &str, model: &str) -> Result<Vec<String>> {
    let mut user = format!("{desc}\n");
    for t in batch {
        let added = t.added.as_deref().and_then(|d| d.get(..10)).unwrap_or("?");
        user += &format!("\n{} | {} | {} | {} | added {}", t.id, t.name, t.artist, t.genre, added);
    }
    let body = json!({
        "model": model, "stream": false, "format": "json",
        "options": {"temperature": 0.2},
        "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": user}]
    });
    let resp: serde_json::Value = ureq::post("http://localhost:11434/api/chat")
        .send_json(body)
        .map_err(|e| format!("Ollama request failed ({e}); start Ollama and run: ollama pull {model}"))?
        .into_json()?;
    let content = resp["message"]["content"].as_str().ok_or("no message.content in Ollama reply")?;
    #[derive(Deserialize)]
    struct Ids {
        ids: Vec<String>,
    }
    let ids: Ids = serde_json::from_str(content)?;
    Ok(keep_known(ids.ids, batch))
}

fn parse_drops(s: &str, len: usize) -> HashSet<usize> {
    s.split(|c: char| c.is_whitespace() || c == ',')
        .filter_map(|w| w.parse::<usize>().ok())
        .filter(|&n| n >= 1 && n <= len)
        .collect()
}

fn review(picks: &[Track]) -> Vec<Track> {
    for (i, t) in picks.iter().enumerate() {
        let added = t.added.as_deref().and_then(|d| d.get(..10)).unwrap_or("?");
        println!("{}. {} — {} (added {})", i + 1, t.name, t.artist, added);
    }
    let line = prompt("Drop which? (e.g. 3 7 12, Enter to keep all) ");
    let drops = parse_drops(&line, picks.len());
    picks.iter().enumerate().filter(|(i, _)| !drops.contains(&(i + 1))).map(|(_, t)| t.clone()).collect()
}

fn valid_date(s: &str) -> bool {
    s.len() == 10 && s.bytes().enumerate().all(|(i, c)| if i == 4 || i == 7 { c == b'-' } else { c.is_ascii_digit() })
}

fn prompt(msg: &str) -> String {
    print!("{msg}");
    io::stdout().flush().ok();
    let mut s = String::new();
    if !matches!(io::stdin().read_line(&mut s), Ok(n) if n > 0) {
        println!("\nCancelled.");
        std::process::exit(0);
    }
    s.trim().to_string()
}

fn create_playlist(name: &str, ids: &[String]) -> Result<usize> {
    let mut args = vec![name];
    args.extend(ids.iter().map(String::as_str));
    Ok(osascript(CREATE_JS, &args)?.parse()?)
}

fn main() -> Result<()> {
    let a = Args::parse();
    for (flag, v) in [("--from", &a.from), ("--to", &a.to)] {
        if !valid_date(v) {
            return Err(format!("{flag} must be YYYY-MM-DD, got {v}").into());
        }
    }
    let tracks: Vec<Track> = read_favorites()?.into_iter().filter(|t| in_range(t, &a.from, &a.to)).collect();
    if tracks.is_empty() {
        println!("No favorites in {}..{}.", a.from, a.to);
        return Ok(());
    }
    let mut ids = Vec::new();
    for b in tracks.chunks(80) {
        ids.extend(pick(b, &a.description, &a.model)?);
    }
    let picks: Vec<Track> = ids
        .iter()
        .filter_map(|id| tracks.iter().find(|t| &t.id == id).cloned())
        .collect();
    if picks.is_empty() {
        println!("No songs matched that description.");
        return Ok(());
    }
    let kept = review(&picks);
    if kept.is_empty() {
        println!("Nothing left to add.");
        return Ok(());
    }
    let default = format!("Mix {} → {}", a.from, a.to);
    let name = prompt(&format!("Playlist name [{default}]: "));
    let name = if name.is_empty() { default } else { name };
    let ids: Vec<String> = kept.into_iter().map(|t| t.id).collect();
    let n = create_playlist(&name, &ids)?;
    println!("Added {n} tracks to \"{name}\"");
    Ok(())
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
        assert!(valid_date("2026-06-01"));
        for bad in ["2026-6-1", "2026/06/01", "", "2026-06-01x"] {
            assert!(!valid_date(bad), "{bad}");
        }
    }

    #[test]
    fn drops() {
        assert_eq!(parse_drops("3 7, 12", 20), HashSet::from([3, 7, 12]));
        assert!(parse_drops("", 5).is_empty());
        assert_eq!(parse_drops("0 2 9", 5), HashSet::from([2]));
        assert!(parse_drops("abc -1 x2", 5).is_empty());
    }
}
