# playlistMixer

Terminal app: describe a vibe and a date range, and a local Ollama model picks and orders songs from your library into a playlist.

## One-time Spotify setup (about 3 minutes)
1. Go to https://developer.spotify.com/dashboard and choose **Create app**.
   - Name: `playlistMixer`
   - Redirect URI: `http://127.0.0.1:8888/callback`
   - API: **Web API**
2. Run `playlistMixer`. It asks for your **Client ID** once, saves it, then opens your browser to log in and approve.
3. If Spotify returns 403, add your Spotify account's email under the app's **User Management**. (The app owner needs Premium.)

Config and token live in `~/.config/playlistMixer/` (`token.json` is mode 0600).

## Usage
- `playlistMixer` uses your Spotify Liked Songs (real dates back to when you liked each song). On the results screen use ←/→ to save to Apple Music or Spotify.
- `playlistMixer --source apple` uses your Apple Music favourites (needs Automation permission for Music.app).
- `--model <name>` picks the Ollama model (default `qwen2.5:7b`). Ollama must be running.
