use crate::{Result, Track};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const REDIRECT: &str = "http://127.0.0.1:8888/callback";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn dir() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var("HOME")?).join(".config/playlistMixer"))
}

fn client_id() -> Result<String> {
    let path = dir()?.join("config.json");
    if let Ok(s) = fs::read_to_string(&path) {
        if let Some(id) = serde_json::from_str::<Value>(&s)?["client_id"].as_str() {
            return Ok(id.to_string());
        }
    }
    print!("Paste your Spotify Client ID: ");
    std::io::stdout().flush()?;
    let mut id = String::new();
    std::io::stdin().read_line(&mut id)?;
    let id = id.trim().to_string();
    if id.is_empty() {
        return Err("No Client ID given".into());
    }
    fs::create_dir_all(dir()?)?;
    fs::write(&path, json!({ "client_id": id }).to_string())?;
    Ok(id)
}

fn random(n: usize) -> Result<Vec<u8>> {
    let mut b = vec![0u8; n];
    fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn parse_callback(request_line: &str) -> Result<(String, String)> {
    let path = request_line.split_whitespace().nth(1).ok_or("bad callback request")?;
    let u = url::Url::parse(&format!("http://127.0.0.1{path}"))?;
    let get = |k: &str| u.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
    if get("error").is_some() {
        return Err("Spotify login was cancelled".into());
    }
    match (get("code"), get("state")) {
        (Some(c), Some(s)) => Ok((c, s)),
        _ => Err("Spotify callback had no code".into()),
    }
}

/// POST to the token endpoint, save the result to token.json, return the access token.
fn token_request(form: &[(&str, &str)], old_refresh: Option<&str>) -> Result<String> {
    let v: Value = ureq::post(TOKEN_URL)
        .send_form(form)
        .map_err(|e| match e {
            ureq::Error::Status(c, r) => format!("Spotify token error {c}: {}", r.into_string().unwrap_or_default()),
            e => e.to_string(),
        })?
        .into_json()?;
    let access = v["access_token"].as_str().ok_or("no access_token in Spotify reply")?.to_string();
    let refresh = v["refresh_token"].as_str().or(old_refresh).unwrap_or("");
    let t = json!({
        "access_token": access, "refresh_token": refresh,
        "expires_at": now() + v["expires_in"].as_u64().unwrap_or(3600),
    });
    fs::create_dir_all(dir()?)?;
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(dir()?.join("token.json"))?;
    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    f.write_all(t.to_string().as_bytes())?;
    Ok(access)
}

pub fn login() -> Result<String> {
    let cid = client_id()?;
    if let Ok(s) = fs::read_to_string(dir()?.join("token.json")) {
        if let Ok(t) = serde_json::from_str::<Value>(&s) {
            if t["expires_at"].as_u64().unwrap_or(0) > now() + 60 {
                if let Some(a) = t["access_token"].as_str() {
                    return Ok(a.to_string());
                }
            }
            if let Some(r) = t["refresh_token"].as_str().filter(|r| !r.is_empty()) {
                let form = [("grant_type", "refresh_token"), ("refresh_token", r), ("client_id", &cid)];
                if let Ok(a) = token_request(&form, Some(r)) {
                    return Ok(a);
                }
            }
        }
    }
    let verifier = URL_SAFE_NO_PAD.encode(random(32)?);
    let state: String = random(16)?.iter().map(|b| format!("{b:02x}")).collect();
    let auth = url::Url::parse_with_params(
        "https://accounts.spotify.com/authorize",
        [
            ("client_id", cid.as_str()),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT),
            ("code_challenge_method", "S256"),
            ("code_challenge", &pkce_challenge(&verifier)),
            ("state", &state),
            ("scope", "user-library-read playlist-modify-private"),
        ],
    )?;
    let listener = TcpListener::bind("127.0.0.1:8888")?;
    println!("Opening Spotify login in your browser… {auth}");
    Command::new("open").arg(auth.as_str()).status()?;
    let (mut stream, line) = loop {
        let (stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut line = String::new();
        if BufReader::new((&stream).take(8192)).read_line(&mut line).is_err() {
            continue;
        }
        if line.starts_with("GET /callback?") {
            break (stream, line);
        }
        let _ = (&stream).write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    };
    let parsed = parse_callback(&line);
    let html = "playlistMixer is logged in — you can close this tab.";
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}", html.len())?;
    let (code, got_state) = parsed?;
    if got_state != state {
        return Err("Spotify login state mismatch".into());
    }
    token_request(
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", REDIRECT),
            ("client_id", &cid),
            ("code_verifier", &verifier),
        ],
        None,
    )
}

fn api(token: &str, method: &str, url: &str, body: Option<Value>) -> Result<Value> {
    for attempt in 0..=3 {
        let req = ureq::request(method, url).set("Authorization", &format!("Bearer {token}"));
        let r = match &body {
            Some(b) => req.send_json(b.clone()),
            None => req.call(),
        };
        match r {
            Ok(resp) => {
                let s = resp.into_string()?;
                return Ok(if s.trim().is_empty() { Value::Null } else { serde_json::from_str(&s)? });
            }
            Err(ureq::Error::Status(429, resp)) if attempt < 3 => {
                let secs = resp.header("Retry-After").and_then(|v| v.trim().parse().ok()).unwrap_or(2);
                std::thread::sleep(std::time::Duration::from_secs(secs));
            }
            Err(ureq::Error::Status(403, _)) => {
                return Err("Spotify refused access. Check the app owner has Premium and your email is under User Management in the Spotify dashboard.".into())
            }
            Err(ureq::Error::Status(c, resp)) => {
                return Err(format!("Spotify error {c}: {}", resp.into_string().unwrap_or_default()).into())
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err("Spotify rate limit: retries exhausted".into())
}

fn parse_likes_page(v: &Value) -> (Vec<Track>, bool) {
    let tracks = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| !i["track"].is_null())
        .map(|i| {
            let t = &i["track"];
            let s = |x: &Value| x.as_str().unwrap_or("").to_string();
            Track {
                id: s(&t["uri"]),
                name: s(&t["name"]),
                artist: t["artists"].as_array().into_iter().flatten().map(|a| s(&a["name"])).collect::<Vec<_>>().join(", "),
                album: s(&t["album"]["name"]),
                genre: String::new(),
                added: i["added_at"].as_str().map(Into::into),
                played: None,
            }
        })
        .collect();
    (tracks, !v["next"].is_null())
}

pub fn read_likes(token: &str) -> Result<Vec<Track>> {
    let mut all = vec![];
    loop {
        let url = format!("https://api.spotify.com/v1/me/tracks?limit=50&offset={}", all.len());
        let (page, has_next) = parse_likes_page(&api(token, "GET", &url, None)?);
        all.extend(page);
        if !has_next {
            return Ok(all);
        }
    }
}

pub fn create_playlist(token: &str, name: &str, uris: &[String]) -> Result<usize> {
    let p = api(
        token,
        "POST",
        "https://api.spotify.com/v1/me/playlists",
        Some(json!({"name": name, "public": false, "description": "Made with playlistMixer"})),
    )?;
    let id = p["id"].as_str().ok_or("no playlist id in Spotify reply")?;
    for chunk in uris.chunks(100) {
        api(token, "POST", &format!("https://api.spotify.com/v1/playlists/{id}/items"), Some(json!({ "uris": chunk })))?;
    }
    Ok(uris.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn callback() {
        let ok = parse_callback("GET /callback?code=abc&state=xyz HTTP/1.1").unwrap();
        assert_eq!(ok, ("abc".to_string(), "xyz".to_string()));
        let e = parse_callback("GET /callback?error=access_denied&state=xyz HTTP/1.1").unwrap_err();
        assert!(e.to_string().contains("cancelled"));
        assert!(parse_callback("GET /callback?state=xyz HTTP/1.1").is_err());
    }

    #[test]
    fn likes_page() {
        let v = json!({"items": [
            {"added_at": "2024-07-01T10:00:00Z", "track": {"uri": "spotify:track:1", "name": "Song",
              "artists": [{"name": "A"}, {"name": "B"}], "album": {"name": "Alb"}}},
            {"added_at": "2024-07-02T10:00:00Z", "track": null}
        ], "next": null});
        let (t, next) = parse_likes_page(&v);
        assert!(!next);
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].id.as_str(), t[0].name.as_str(), t[0].artist.as_str(), t[0].album.as_str()),
            ("spotify:track:1", "Song", "A, B", "Alb"));
        assert_eq!(t[0].added.as_deref(), Some("2024-07-01T10:00:00Z"));
        assert!(t[0].genre.is_empty() && t[0].played.is_none());
    }
}
