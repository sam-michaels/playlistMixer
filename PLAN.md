# playlistMixer: playlists from your Apple Music favorites, chosen by mood

## Context
You want playlists built from songs you favorited during a period (for example, this summer), filtered by a description of how that time felt. Apple Music can't list favorites by date or by mood. playlistMixer is a local Rust command-line tool. It reads your library straight from Music.app, has a small local LLM (through Ollama) pick the songs that fit, lets you remove any you don't want, and then creates the playlist in Music.app. Nothing leaves your Mac.

What I checked on this Mac (macOS 27.0.1): Music.app's scripting interface exposes `favorited`, `date added`, `played date` and `genre` for each track. Ollama is at `/opt/homebrew/bin/ollama`. Cargo is **not** installed.

**Known limit:** Apple doesn't record *when* you favorited a song. The date filter keeps a song if it was added to your library **or** last played within the range.

---

## Agent workflow (how this gets built)

| Stage | Agent | Model / effort | Output |
|---|---|---|---|
| 1. Plan | Main session (this one) | Opus 5.5, effort set per task | This file. **You review it before anything is built.** |
| 2. Implement | `implementer` subagent | Sonnet 5.5, effort `high` | Code written exactly to the spec below |
| 3. Review | Codex (`codex:codex-rescue` subagent) | Codex default | List of bugs and issues |
| 4. Triage | Main session | Opus 5.5 | For each Codex finding: I check it against the code, show you the bug, and walk you through it (real or false alarm, and the fix). Fixes go back to `implementer`. |
| 5. Explain | `explainer` subagent ("function" agent) | Sonnet 5.5, effort `medium`, read-only tools | Overview of each function: what it does and why it was built that way. Runs after stage 2 and again after any fixes in stage 4. |

Setup before stage 2:
- Create `~/playlistMixer/.claude/agents/implementer.md` (frontmatter: `model: sonnet`, `effort: high`, tools: Read/Write/Edit/Bash). Its prompt: follow `PLAN.md` exactly, don't add features, run `cargo test` before finishing, and report anything in the spec that turned out to be wrong.
- Create `~/playlistMixer/.claude/agents/explainer.md` (frontmatter: `model: sonnet`, `effort: medium`, tools: Read/Grep/Glob only). Its prompt: read `PLAN.md` and `src/`, then write a per-function overview with the reasoning behind each design choice, pointing to the plan where it applies.
- Copy this plan to `~/playlistMixer/PLAN.md` so the subagents work from the same spec.
- Run `codex:setup` to confirm the Codex CLI works before stage 3.

---

## Spec for the implementer

### Usage
```
playlist-mixer --from 2026-06-01 --to 2026-08-31 [--model qwen2.5:7b] "anxious in June, carefree road trips in July, nostalgic end of August"
```
1. Read favorited tracks from Music.app, then filter to the date range
2. Send them to Ollama in batches of 80 with your description; keep the IDs it returns
3. Print a numbered list: `N. Title — Artist (added YYYY-MM-DD)`
4. Prompt: `Drop which? (e.g. 3 7 12, Enter to keep all)`
5. Prompt: `Playlist name [Summer 2026]:`, then create the playlist and print `Added N tracks to "<name>"`

### Files
- `~/playlistMixer/Cargo.toml`: package `playlist-mixer`, dependencies `serde` (derive), `serde_json`, `ureq` (json feature), `clap` (derive). No other crates.
- `~/playlistMixer/src/main.rs`: the whole program.

### `src/main.rs` contents
```rust
#[derive(Deserialize, Clone)]
struct Track { id: String, name: String, artist: String, album: String, genre: String,
               added: Option<String>, played: Option<String> } // ISO-8601 strings
```

**`read_favorites() -> Result<Vec<Track>>`**: runs `osascript -l JavaScript -e <JS>` and parses stdout as JSON. JS (fetches whole columns at once, which is fast):
```js
const M = Application('Music');
const t = M.libraryPlaylists[0].tracks.whose({favorited: true});
const id=t.persistentID(), n=t.name(), a=t.artist(), al=t.album(), g=t.genre(), ad=t.dateAdded(), pl=t.playedDate();
JSON.stringify(id.map((x,i)=>({id:x,name:n[i],artist:a[i],album:al[i],genre:g[i],
  added: ad[i]?ad[i].toISOString():null, played: pl[i]?pl[i].toISOString():null})))
```
If osascript exits non-zero, show its stderr and mention the macOS Automation permission (System Settings → Privacy & Security → Automation).

**`in_range(t: &Track, from: &str, to: &str) -> bool`**: true if `added[..10]` or `played[..10]` falls in `from..=to`, compared as strings (ISO dates sort correctly as text). Add `// ponytail: UTC dates, can be off by a few hours at the range edges; parse to local time if that matters`.

**`pick(batch: &[Track], desc: &str, model: &str) -> Result<Vec<String>>`**: `POST http://localhost:11434/api/chat` with `{"model", "stream": false, "format": "json", "options": {"temperature": 0.2}, "messages": [system, user]}`.
- System: `You choose songs for a playlist. The user describes how a period of their life felt. From the numbered song list, return JSON {"ids": [...]} with the ids of songs whose mood, sound or lyrics fit the description. Use what you know about each song. Return only ids from the list.`
- User: the description, a blank line, then one line per track: `id | title | artist | genre | added YYYY-MM-DD`.
- Parse `message.content` as `{"ids": [String]}` and pass the result through `keep_known`.
- If Ollama can't be reached, say `start Ollama and run: ollama pull <model>`.

**`keep_known(ids: Vec<String>, batch: &[Track]) -> Vec<String>`**: drop any ID not in the batch (small models sometimes make them up) and remove duplicates.

**`review(picks: &[Track]) -> Vec<Track>`**: print the numbered list, read a line from stdin, parse the numbers separated by whitespace or commas, ignore anything that's invalid or out of range, and return the tracks that remain.

**`create_playlist(name: &str, ids: &[String]) -> Result<usize>`**: runs `osascript -l JavaScript -e <JS> <name> <id...>`:
```js
function run(argv){
  const M=Application('Music'), lib=M.libraryPlaylists[0];
  const p=M.UserPlaylist({name: argv[0]}).make();
  argv.slice(1).forEach(id=>{ const t=lib.tracks.whose({persistentID:id}); if(t.length) M.duplicate(t[0],{to:p}); });
  return p.tracks.length;
}
```
Returns the track count it printed.

**`main`**: parse args with clap, then read, filter, pick in `chunks(80)`, review, ask for the name, create the playlist. If no tracks are in range, or no picks come back, print a message and exit 0.

**Tests (`#[cfg(test)]`)**:
- `in_range`: added date inside the range, played date inside, both outside, and `None` values
- `keep_known`: drops made-up IDs and duplicates
- The review-input parser (split it into `parse_drops(&str, len) -> HashSet<usize>`): `"3 7, 12"`, empty input, out-of-range numbers, junk text

Skipped for now: web UI, caching LLM results, splitting the playlist by month. Add them later if the terminal flow feels clunky.

## Environment setup (stage 2, first step)
- `brew install rustup && rustup-init -y`, then `source ~/.cargo/env`
- `ollama pull qwen2.5:7b`
- The first run triggers a macOS prompt asking to let Terminal control Music. Allow it.

## Verification
1. `cargo test`: all tests pass.
2. `cargo run -- --from 2026-06-01 --to 2026-08-31 "happy"`: the number of favorites read looks right compared with Music.app (sort Favorites by Date Added).
3. Run it end to end, keep all picks, and name the playlist `playlistMixer test`. Check that it appears in Music.app with the listed tracks, then delete it by hand.
4. The Codex review is triaged with you (stage 4), and the explainer's overview is delivered (stage 5).
