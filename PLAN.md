# playlistMixer v2: full-screen terminal app (Mole-style)

## Context
Today `playlistMixer` asks its questions one line at a time. You want it to feel like **Mole**: you type `playlistMixer`, a full-screen app opens with a menu, and you fill in boxes for the date range and a **vibe profile** (the moods, and how the playlist should flow across those dates). The engine stays the same: reading Music.app, picking with Ollama, and creating the playlist are already tested. This change replaces the line-by-line prompts with a full-screen app (TUI) and adds one feature: **ordering the picked songs to follow the vibe's flow**.

Your choices: a menu screen first; dates typed as **DD/MM/YYYY**; the model orders songs by flow; the results screen has a checklist you can toggle plus a rename box.

## Workflow (same as before)
1. **Plan:** this file, which you review → 2. **Implement:** Sonnet subagent, with the role from `.claude/agents/implementer.md` → 3. **Review:** Codex → 4. **Triage:** I check each finding and walk you through it → 5. **Explain:** the explainer writes an overview of the new UI code.

The uncommitted line-prompt changes in `src/main.rs` (`ask_date`, `Run`, optional clap args) are replaced by this work. Keep the `[[bin]] name = "playlistMixer"` entry in `Cargo.toml`.

---

## Spec for the implementer

### Dependencies (`Cargo.toml`)
Add `ratatui = "0.29"`. Use its built-in crossterm (`ratatui::crossterm`) and `ratatui::init()` / `ratatui::restore()`. These handle raw mode, the alternate screen and restoring the terminal after a crash. Keep `clap` **only** for `--model` (default `qwen2.5:7b`). No other new crates.

### Files
- `src/main.rs`: the engine plus `main`. Keep these unchanged: `Track`, `READ_JS`, `CREATE_JS`, `osascript`, `read_favorites`, `in_range`, `keep_known`, `pick`, `create_playlist`. **Delete** `prompt`, `review`, `parse_drops`, `ask_date`, `Run`, `dmy`, and the `drops` test.
- `src/ui.rs`: new. Holds all TUI state, drawing and key handling.

### Engine changes (`src/main.rs`)
- **Dates are DD/MM/YYYY.** `valid_date(s)` checks the shape `DD/MM/YYYY`: 10 bytes, `/` at positions 2 and 5, digits everywhere else, day 01–31, month 01–12. Add `iso(s) -> String`, which turns `30/04/2026` into `2026-04-30`. `in_range` keeps taking ISO strings.
- **`form_error(from, to, vibe) -> Option<String>`** checks the form before generating:
  - `"From must be DD/MM/YYYY"`
  - `"To must be DD/MM/YYYY"`
  - `"From is after To"` (compare the `iso()` values)
  - `"Describe a vibe first"` (if the vibe is empty after trimming)
- **`SYSTEM` prompt:** replace "The user describes how a period of their life felt" with "The user gives a vibe profile: the moods of a period of their life and how the playlist should flow".
- **`order(picks: &[Track], vibe, model) -> Result<Vec<String>>`** is a new function. It makes one Ollama call shaped like `pick` (same request settings, same track-line format). System prompt: `Arrange these songs into a playlist whose sequence follows the vibe profile's flow from start to end. Return JSON {"ids": [...]} containing every id exactly once, in playing order.` The result goes through `fill_order`.
- **`fill_order(ids: Vec<String>, picks: &[Track]) -> Vec<String>`** applies `keep_known`, then appends any picks the model left out, in their original order, so no song is ever lost. Add `// ponytail: one ordering call for all picks; batch it if picks ever exceed ~150`.
- **`main`:**
  1. Parse `--model`.
  2. Print `Reading your Music library…` in the plain terminal.
  3. Run `read_favorites()?`. On an error, print it and exit **before** the TUI starts, so the permission hint stays readable.
  4. Call `ui::run(tracks, model)`.

### UI (`src/ui.rs`)
`pub fn run(tracks: Vec<Track>, model: String) -> Result<()>` calls `ratatui::init()`, runs the event loop, then calls `ratatui::restore()`. It must restore the terminal on every exit path, including errors.

**State:**
```rust
enum Screen { Menu, Form, Results, Done(String) }
struct App {
    screen: Screen, menu_sel: usize,              // 0 New playlist, 1 Quit
    from: String, to: String, vibe: String,
    focus: usize,                                  // Form: 0 from, 1 to, 2 vibe, 3 [Generate]
    error: Option<String>, status: Option<String>, // red error line / "Working…" line
    tracks: Vec<Track>, model: String,
    picks: Vec<Track>, checked: Vec<bool>, cursor: usize,
    name: String, name_focus: bool,                // Results
    quit: bool,
}
```

**Screens.** Every screen shows:
- a header banner: ` ░░ playlistMixer ░░ ` with `N favorites · model <model>` underneath
- a footer with the key hints for that screen
- the error line in red, if there is one

Boxes use `BorderType::Rounded`. The focused box has a cyan border and the others are dark gray. Draw a `█` cursor at the end of the focused text field.

| Screen | Content | Keys |
|---|---|---|
| **Menu** | `› New playlist` / `Quit` | ↑↓ (and j/k) move · Enter select · q/Esc quit |
| **Form** | Row 1: **From** box and **To** box side by side. Gray placeholder `DD/MM/YYYY` when empty. Below: **Vibe profile** box (multi-line, wrapped, at least 6 rows) with a placeholder: `e.g. Start restless and moody in June, build to carefree road-trip energy in July, end nostalgic`. Then a `[ Generate ]` button, highlighted when focused. | Tab / Shift-Tab cycle focus · date boxes: digits and `/` only, max 10 · Backspace · Enter: date box → next box, vibe → newline, button → generate · Esc → Menu |
| **Results** | **Picks (n)** list of `[x] Title — Artist   DD/MM` with a `›` marker on the cursor row and scrolling via `ListState`. Below it, the **Playlist name** box, prefilled with `"{from} - {to}"`. | ↑↓ move · Space toggle · Tab switch between list and name · typing edits the name when it has focus · Enter create · Esc → Form, keeping the inputs |
| **Done(msg)** | Centered message, e.g. `Added 14 tracks to "01/06/2026 - 31/08/2026"` | any key → Menu |

Ctrl-C quits from any screen.

**Generate** (Enter on the button):
1. If `form_error` returns something, set `error` and stay on the form.
2. Filter `tracks` with `in_range(t, iso(from), iso(to))`. If none are in range, show the error `No favorites between those dates`.
3. For each `chunks(80)` batch, set `status = "Picking songs… batch i/n"`, call `terminal.draw`, then call `pick` (blocking is fine, since redrawing before each call keeps the status visible). Then set `status = "Arranging by flow…"`, redraw, and call `order`.
4. If there are no picks, show the error `No songs matched that vibe`. Otherwise go to Results with every song checked.
5. If Ollama fails, put the error message (which already says "start Ollama…") in `error`, go back to the Form, and keep the inputs.

**Create** (Enter on Results):
- If nothing is checked, show the error `Nothing selected`.
- Otherwise set `status = "Creating playlist…"`, redraw, and call `create_playlist(name or default, checked ids in displayed order)`.
- Success goes to `Done(msg)`. An error sets `error` and stays on Results.

**Make key handling testable:** put it in `fn on_key(&mut self, key: KeyEvent) -> Action`, where `enum Action { None, Generate, Create }`. Pure state changes happen inside `on_key`. The two blocking actions are returned as an `Action` so the loop can run them with access to `terminal`.

### Tests (`#[cfg(test)]`)
- **`main.rs`:** keep `range` and `known`. Update `dates` for DD/MM/YYYY, and test `iso("30/04/2026") == "2026-04-30"`. Rejected inputs: `"2026-04-30"`, `"32/01/2026"`, `"01/13/2026"`, `"1/4/2026"`, `""`. Add `form_error` cases (each of the 4 messages, plus `None` for valid input) and `fill_order` cases (made-up IDs dropped, missing picks appended, no duplicates).
- **`ui.rs`:** one test that drives `on_key`:
  - Menu Enter → Form
  - typing `01/06/2026` into From, Tab, typing `31/08/2026`
  - Tab, typing a vibe, Tab, Enter → returns `Action::Generate`
  - a letter typed into a date box is ignored

---

## Verification
1. `cargo build` with no warnings, and all `cargo test` tests pass. I (the main session) check this myself.
2. Codex review, then triage with you.
3. Install with `cargo install --path ~/playlistMixer`. Start Ollama and run `ollama pull qwen2.5:7b` (one time).
4. **You** run `playlistMixer` from `~`:
   1. Menu → New playlist.
   2. Enter 01/06/2026 – 31/08/2026 and a vibe, then Generate. Check the status messages and the ordered picks.
   3. Untick one song, keep the default name, press Enter. Check that the playlist appears in Music.app.
   4. Esc/q exits and your terminal is back to normal.
5. Commit and push once you're happy.

---

# v2.1 fix: read all favourites + "any date"

## Context
Bug: `READ_JS` and `CREATE_JS` only use the Library (`libraryPlaylists[0]`), which holds 56 tracks (42 favourited). The user's "Favourite Songs" smart playlist has 1,218 tracks, most of them Apple Music catalog songs that aren't in the Library. This caused only 42 to be read and an empty playlist to be created. Verified: songs looked up in that playlist by persistentID can be `duplicate`d into a new playlist. Only the newest 42 favourites have `dateAdded`. The other 1,176 have null dates, so leaving both date boxes empty means "any date, match by vibe only".

## Spec
1. **READ_JS**: find the source playlist by name: `const names=M.userPlaylists.name(); const i=names.findIndex(n=>/^Favou?rite Songs$/.test(n));`. Source tracks are `M.userPlaylists[i].tracks.whose({favorited:true})` if found, else `M.libraryPlaylists[0].tracks.whose({favorited:true})`. Fetch the columns the same way as now and dedupe by persistentID in JS.
2. **CREATE_JS**: find the same playlist the same way. For each id, look it up in the Favourite Songs tracks first (if found), then in the library tracks, and `duplicate` the first match. Still return `p.tracks.length`.
3. **Ollama context**: add `"num_ctx": 16384` to `options` in both `pick` and `order`. The Ollama default (2048–4096) truncates an 80-track batch.
4. **Any date**: if both From and To are empty, there is no date filter (the pool is all tracks).
   - `form_error`: both empty → only the vibe check. Exactly one empty → `"Fill both dates, or leave both empty for any date"`. Otherwise same as now.
   - `generate`: when there are no dates, the pool is all `tracks`.
   - Default playlist name when there are no dates: the first line of the vibe, trimmed to 40 chars. Otherwise `"{from} - {to}"` as now.
   - Placeholders: From is `DD/MM/YYYY (blank = any)`, To is `DD/MM/YYYY`.
5. **Order cap**: in `generate`, only call `order` when picks ≤ 150. Otherwise keep the pick order and set the status to `Too many picks to arrange by flow — kept in favourite order`. Update the ponytail comment to match.
6. **Header**: `{N} favourites · {D} with dates · model {m}`, where D counts tracks with `added.is_some()`.
7. **Tests**: `form_error` covers both-empty + vibe → None, and one-empty → the new message. The `ui.rs` `keys` test still passes.

---

# playlistMixer v3: Spotify Liked Songs as the main source

## Context
Apple Music only gives real "favourited" dates for the 42 songs you liked after you moved from Spotify (23 June 2026). Your older ~1,176 favourites were transferred and have no dates. Your **Spotify Liked Songs** have a real `added_at` date for every song, so v3 makes Spotify the default source. Date ranges then work across your whole history, back to 2021 and earlier. The Apple Music source stays available (`playlistMixer --source apple`). On the results screen you choose where to save each playlist: **Apple Music** or **Spotify**.

Facts checked (Oct 2026):
- Since Feb 2026, Spotify's API works only if the app owner has **Premium** (you do). Personal apps are limited to 5 users.
- `GET /v1/me/tracks` (scope `user-library-read`, 50 per page) still returns `added_at`.
- Playlist creation is now `POST /v1/me/playlists`. Adding songs is now `POST /v1/playlists/{id}/items` (max 100 per call).
- The redirect URI must be `http://127.0.0.1:PORT/...`. `localhost` is not allowed.
- Spotify no longer gives new apps audio features or mood data, so the Ollama mood matching stays.

## Workflow
Same as before: this plan (you review) → Sonnet implementer → Codex review → I triage and walk you through each finding → the explainer writes an overview.

## One-time setup (you, about 3 minutes; written into the README the implementer creates)
1. Go to https://developer.spotify.com/dashboard and choose **Create app**.
   - Name: `playlistMixer`
   - Redirect URI: `http://127.0.0.1:8888/callback`
   - API: **Web API**
2. Run `playlistMixer`. It asks for your **Client ID** once, saves it, then opens your browser so you can log in and approve.
3. If Spotify returns 403, add your Spotify account's email under the app's **User Management**.

---

## Spec for the implementer

### Dependencies (`Cargo.toml`)
Add `sha2 = "0.10"`, `base64 = "0.22"` and `url = "2"` (`url` is already in the dependency tree through ureq). No other crates. Randomness comes from reading `/dev/urandom`.

### Files
- `src/spotify.rs` (new): login, reading likes, creating playlists.
- `src/main.rs`: add the `--source spotify|apple` flag (default `spotify`), `match_to_apple`, and the short-number change. Everything Apple-related stays as it is.
- `src/ui.rs`: add a "Save to" choice on the results screen, plus small text changes.
- `README.md` (new): setup steps and usage. Keep it short.

### `src/spotify.rs`
**Config:** the folder is `~/.config/playlistMixer/` (from the `HOME` env var).
- `config.json` holds `{"client_id": "..."}`. If it's missing, ask `Paste your Spotify Client ID: ` in the plain terminal before the TUI starts, then save it.
- `token.json` holds `{"access_token", "refresh_token", "expires_at"}` (unix seconds). Write it with file mode **0600** using `OpenOptionsExt::mode`.

**`pub fn login() -> Result<String>`** returns a valid access token.
- If `token.json` exists and hasn't expired (with a 60-second margin), use the saved token.
- If it has expired, refresh it: `POST https://accounts.spotify.com/api/token` with form fields `grant_type=refresh_token`, `refresh_token` and `client_id`. Save the new token, and keep the old `refresh_token` if the response doesn't include one.
- If the refresh fails or there's no token yet, run the PKCE login:
  - `verifier`: 32 bytes from `/dev/urandom`, base64url-encoded without padding (43 characters).
  - `challenge`: base64url-encoded SHA-256 of `verifier`, without padding. Put this in `fn pkce_challenge(verifier) -> String` so it can be tested.
  - `state`: 16 random bytes, hex-encoded.
  - Auth URL: `https://accounts.spotify.com/authorize` with `client_id`, `response_type=code`, `redirect_uri=http://127.0.0.1:8888/callback`, `code_challenge_method=S256`, `code_challenge`, `state` and `scope=user-library-read playlist-modify-private`. Build it with `url::Url::parse_with_params`.
  - Print `Opening Spotify login in your browser…` along with the URL, then run `open <url>`.
  - `TcpListener::bind("127.0.0.1:8888")`, then accept one connection and read the request line (`GET /callback?code=…&state=… HTTP/1.1`).
  - Parse it with `fn parse_callback(request_line) -> Result<(code, state)>`, which is testable. An `error=` parameter becomes an error saying "Spotify login was cancelled".
  - Reply `HTTP/1.1 200 OK` with the HTML text `playlistMixer is logged in — you can close this tab.`
  - Check that `state` matches.
  - Exchange the code: `POST /api/token` with the form fields `grant_type=authorization_code`, `code`, `redirect_uri`, `client_id` and `code_verifier`. Save the token.

**`fn api(token, method, url, body: Option<Value>) -> Result<Value>`** is the one helper every Spotify call goes through.
- On a **429**, sleep for `Retry-After` seconds (default 2) and retry, up to 3 times.
- On a **403**, return the error `Spotify refused access. Check the app owner has Premium and your email is under User Management in the Spotify dashboard.`
- Other errors return the status and body.

**`pub fn read_likes(token) -> Result<Vec<Track>>`** pages through `GET https://api.spotify.com/v1/me/tracks?limit=50&offset=N` until `next` is null. It maps each item to `Track`:

| Track field | Value |
|---|---|
| `id` | `track.uri` |
| `name` | `track.name` |
| `artist` | all `artists[].name`, joined with `", "` |
| `album` | `album.name` |
| `genre` | `""` |
| `added` | `Some(added_at)` |
| `played` | `None` |

Skip items where `track` is null (local or unavailable songs). Put the mapping of one page in `fn parse_likes_page(&Value) -> (Vec<Track>, bool has_next)` so it can be tested.

**`pub fn create_playlist(token, name, uris) -> Result<usize>`**:
1. `POST /v1/me/playlists` with `{name, "public": false, "description": "Made with playlistMixer"}`.
2. `POST /v1/playlists/{id}/items` with `{"uris": chunk}` for each `chunks(100)`.
3. Return the number of songs added.

### `src/main.rs` changes
- **Flags:** `--source` (`spotify` by default, or `apple`) and `--model`.
- **`main`:**
  - `spotify`: print `Logging in to Spotify…` → `login()`, then `Loading your liked songs…` → `read_likes`, then `ui::run(tracks, model, Some(token))`.
  - `apple`: works as now and passes `None`.
- **Short numbers (speed):** in `pick` and `order`, list songs as `N | title | artist | genre | added YYYY-MM-DD`, with N starting at 1 within the list sent. Ask for `{"ids": [numbers]}` and map the numbers back to track IDs.
  - Put this in `fn ids_from_numbers(nums: Vec<serde_json::Value>, batch: &[Track]) -> Vec<String>`. It accepts both numbers and numeric strings, ignores anything out of range, and the result still goes through `keep_known` and `fill_order`.
  - Update both system prompts to say "return the numbers of the songs".
  - The model writes about 4× less, so each batch runs several times faster.
- **`match_to_apple(picks: &[Track], apple: &[Track]) -> (Vec<String> apple_ids, Vec<String> missing_names)`** matches songs by `norm(title) + "|" + norm(first artist)`.
  - `norm`: lowercase, cut everything from the first ` (`, ` [` or ` - ` (this removes "(feat. …)", "- 2011 Remaster" and similar), keep only letters and digits, and trim.
  - The first artist is the text before the first `,` or `&`.
  - It returns the Apple IDs in pick order, plus the `"Title — Artist"` of songs with no match.

### `src/ui.rs` changes
- `run(tracks, model, spotify_token: Option<String>)`, stored in `App`.
- The header shows `{N} liked songs on Spotify · {model}` or, for Apple, `{N} favourites · {D} with dates · {model}`.
- **Results screen:** when the source is Spotify, add a **Save to** line under the name box: `Save to:  ‹ Apple Music ›  Spotify`. ←/→ switches between them (default Apple Music). The footer hint mentions ←→.
  - **Apple Music:** status `Matching to your Apple Music favourites…`, then `read_favorites()` and `match_to_apple`, then the existing `create_playlist` (Apple).
    - Done message: `Added 52 of 60 to "<name>" in Apple Music`. If any songs are missing, add a second line `Not found in Apple Music: ` with up to 5 names, then `+N more`.
    - If nothing matched, show the error `None of these songs are in your Apple Music favourites` and stay on Results.
  - **Spotify:** status `Creating Spotify playlist…`, then `spotify::create_playlist`. Done: `Added 60 tracks to "<name>" on Spotify`.
- Source Apple: there's no Save-to line, and it behaves as now.
- Rename the Apple `create_playlist` in `main.rs` to `create_apple_playlist` so the two can't be confused.
- With the Spotify source, the From placeholder `DD/MM/YYYY (blank = any)` stays the same, because every song now has a real date.

### Tests (`#[cfg(test)]`; all existing tests stay)
- `pkce_challenge("dBjftJeZ4CVP-mJ92K5o5mgL3BJLtGYcZYuJ3U6Js0w") == "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuEJSstw-cM"` (the RFC 7636 example).
- `parse_callback`: one success line, one `error=access_denied`, and one line missing `code`.
- `parse_likes_page`: a small inline JSON page with 2 items (one with `track: null`) and `next: null` gives 1 track with the correct fields and `has_next == false`.
- `ids_from_numbers`: `[1, "2", 99, "x"]` with a batch of 3 → the IDs of tracks 1 and 2.
- `match_to_apple`: `"Mr. Brightside"` by `"The Killers"` matches Apple's `"Mr. Brightside - 2004 Remaster"`; `"Song (feat. X)"` by `"A, B"` matches `"Song"` by `"A"`; an unknown song ends up in `missing`.

### Skipped (for later)
- A saved mood index for searches of about 10 seconds. Add it if "any date" searches over all likes still feel slow.
- Saving to Spotify when the source is Apple, which would need Spotify search (now capped at 10 results per query).
- Caching likes between runs. Loading takes a few seconds.

---

## Verification
1. `cargo build` with zero warnings, and all tests pass. I check this myself.
2. You do the one-time Spotify setup, then I run `playlistMixer` up to the login. You approve in the browser. I confirm `token.json` exists with mode 0600, and that the liked-song count matches your Spotify Liked Songs total.
3. Codex review, then triage with you.
4. You make a summer 2024 playlist: Generate, check the picks have dates in range, save to **Apple Music**, and check the "not found" list. Then make another and save it to **Spotify**. Check both appear.
5. Restart `playlistMixer`. It should not ask you to log in again, because the token refresh works.
6. Commit and push v2, v2.1 and v3, then `cargo install --path ~/playlistMixer`.

---

# playlistMixer v4: play songs in Spotify + instant startup

## Context
You want to hear songs while you pick through results, in the Spotify app, and you want the app to stay fast. Two changes:
1. **Press `p` to preview** the highlighted song in Spotify.app, starting about a third of the way in (roughly the chorus). Press `p` again to pause.
2. **Cache your likes locally**, so startup makes about one request instead of about 25.

While planning I also found a **real bug** in `read_likes` (`src/spotify.rs:208`). It pages with `offset = all.len()`, but `all` excludes the unavailable songs it skipped (`track: null`). The offset falls behind and pages get fetched twice, which duplicates songs. That likely explains why the count went from 1,201 to 1,249. The fix: follow Spotify's `next` URL and remove duplicates by ID.

What I checked on your Mac: `/Applications/Spotify.app` can be scripted, and it supports `play track "<uri>"`, `playpause`, `player position` (seconds, settable) and `current track`'s `duration` / `id`. Local scripting needs **no API call, no new login scope and no network**.

## Workflow
This plan (you review) → Sonnet implementer → Codex review → I triage with you → explainer.

---

## Spec for the implementer

### 1. Preview playback (`src/ui.rs`, plus a new `src/spotify.rs` function)
- **`pub fn play_preview(uri: &str)`** in `src/spotify.rs`. It is **non-blocking**: it runs `std::thread::spawn` and calls `osascript -e <script> <uri>` in the background (`Command::new("osascript")...status()`), ignoring errors, so the UI never waits. Script, using an AppleScript `on run argv` handler:
  ```applescript
  on run argv
    set u to item 1 of argv
    tell application "Spotify"
      play track u
      repeat 40 times
        if (id of current track) is u then exit repeat
        delay 0.05
      end repeat
      set d to duration of current track
      if d > 10000 then set d to d / 1000 -- Spotify reports ms despite the dictionary saying seconds
      set player position to d / 3
    end tell
  end run
  ```
- **`pub fn toggle_pause()`** runs `tell application "Spotify" to playpause`, non-blocking in the same way.
- **`App` gets `playing: Option<usize>`**, the index in `picks` of the previewed song.
- **Results keys** (list focused, and only when the source is Spotify, i.e. `token.is_some()`):
  - `p` on a different song than `playing` → `play_preview(&picks[cursor].id)` and set `playing = Some(cursor)`.
  - `p` on the same song → `toggle_pause()`.
  - In the name box, `p` still types into the name, as it does now.
- **Drawing:** the row whose index equals `playing` shows ` ♪` after the date, in Spotify green (`SPOTIFY_GREEN`). The list footer hint gains `p play/pause`. Reset `playing = None` whenever new results load in `generate`.
- **The Apple source is unchanged.** There's no `p` there, because you only use Spotify.
- **No playback API scopes and no re-login.**

### 2. Likes cache and paging fix (`src/spotify.rs`)
- **`Track`** (`src/main.rs:21`) also derives `Serialize`.
- **Cache file** `~/.config/playlistMixer/likes.json` holds `{"total": <Spotify's total>, "tracks": [Track…]}`, newest first. Write it the plain way; it isn't secret.
- **`parse_likes_page`** returns `(tracks, next: Option<String>, total: u64, raw_ids: Vec<Option<String>>)`, or something equivalent. The **raw item count** (including null tracks) and the URIs are needed so the incremental check is exact. Keep the existing test passing (update it as needed).
- **`read_likes(token)`**:
  1. Load the cache. If it's missing or corrupt, do a **full fetch**.
  2. **Incremental:** follow `next` URLs from `…/me/tracks?limit=50` and collect items until you reach a track whose URI is the cache's newest URI. Count every raw item before that point as `new_raw`, then stop.
     - If `total == cache.total + new_raw` → the result is `new tracks ++ cache.tracks`.
     - Otherwise (you unliked something, or the newest cached song wasn't found within 5 pages) → **full fetch**.
  3. **Full fetch:** follow `next` URLs to the end, never computing the offset yourself, and **remove duplicates by URI** while keeping the first occurrence.
  4. Save the cache with the latest `total`, and return.
- Put the decision in **`fn merge_likes(new: Vec<Track>, new_raw: u64, total: u64, cache: Cache) -> Option<Vec<Track>>`**. It returns `None` when a full fetch is needed, so it can be tested without the network.

### Tests (`#[cfg(test)]`, all existing tests stay)
- `merge_likes`:
  - 2 new + cache of 3 with matching totals → 5, in order.
  - totals don't match → `None`.
  - 0 new, same total → the cache unchanged.
- Full-fetch duplicate removal: a helper `dedupe_by_id(Vec<Track>)` removes a repeated URI and keeps order.
- `results` key test: with `token = Some(..)`, `p` sets `playing = Some(cursor)`. Calling `play_preview` from a test is fine because errors are ignored, but **guard the actual spawn with `#[cfg(not(test))]`** so tests never touch Spotify.

### Out of scope
Apple Music preview, auto-preview while scrolling, and stopping playback when you quit (music keeps playing, the same as using Spotify itself).

---

## Verification
1. `cargo build` with zero warnings and `cargo test` all green. I check this myself.
2. **First startup after the change** does a full fetch and writes `likes.json`. I compare the song count with **your Spotify Liked Songs total**, and check that duplicates are gone (the count should drop from 1,249 if the paging bug was duplicating songs).
3. **Second startup:** I time it. Loading likes should take well under a second, compared with a few seconds now.
4. **You** open Results, press `p` on a song, and hear it in Spotify about a third of the way in, with ♪ shown next to it. `p` again pauses. `p` on another song switches to it.
5. Codex review → triage → explainer → commit, push and `cargo install`.
