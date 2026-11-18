# External MCP server

Agents outside AI-Track (Claude Desktop, Claude Code in a terminal, Cursor, any MCP client) can read and edit the project that is open in a running app. It is the same tool set as the built-in assistant (`get_project`, `create_track`, `write_clip`, `add_device`, `set_params`, `set_automation`, `generate_part`, ...), plus three resources (`aitrack://project`, `aitrack://pending`, `aitrack://guide`) and three prompts (`build_part`, `mix_check`, `master_song`).

## Turn it on

Preferences > **MCP server** > *Let external agents use AI-Track*. It is off by default.

- The server listens on `127.0.0.1` only (default port 47615, changeable on the same page). It never binds another address.
- Every client must send the install's token: `Authorization: Bearer <token>`. The page shows it masked, with **Show**, **Copy** and **New token** (old clients stop working at once). It lives in `mcp-token` in the config folder (`~/Library/Application Support/AI-Track` on macOS, `%APPDATA%\AI-Track` on Windows), readable by you only.
- The status bar shows `MCP: 1 client connected` while it runs, and the page lists the latest calls.

## What edits do

External edits are never applied behind your back. They are collected into a draft and offered in the assistant as **one proposal** that you accept (one undo step) or discard. The agent can keep building on the draft: its reads show the draft, not the untouched project. Deleting anything (`delete_track`, `clear_bars`, `remove_device`) always waits for you.

*Apply external edits directly* applies each call at once as its own undo step. Deletions still ask.

## Claude Desktop

Preferences > MCP server > Claude Desktop > **Copy config**, and paste it into `claude_desktop_config.json` (Settings > Developer > Edit Config). It runs the `aitrack-mcp` shim that ships next to the app; the shim finds the port and token itself, so the snippet holds no secret:

```json
{ "mcpServers": { "aitrack": { "command": "/Applications/AI-Track.app/Contents/MacOS/aitrack-mcp", "args": [] } } }
```

Restart Claude Desktop. AI-Track must be running with the server on.

## Claude Code

Over HTTP (the copied command includes your token):

```sh
claude mcp add --transport http aitrack http://127.0.0.1:47615/mcp --header "Authorization: Bearer <token>"
```

Or over stdio through the shim, with no token in the command:

```sh
claude mcp add aitrack -- /Applications/AI-Track.app/Contents/MacOS/aitrack-mcp
```

## Cursor

Preferences > MCP server > Cursor > **Copy config**, into `~/.cursor/mcp.json` (or the project's `.cursor/mcp.json`):

```json
{ "mcpServers": { "aitrack": { "url": "http://127.0.0.1:47615/mcp", "headers": { "Authorization": "Bearer <token>" } } } }
```

## Other clients

Streamable HTTP clients: URL `http://127.0.0.1:<port>/mcp` and the bearer header. Stdio-only clients: run `aitrack-mcp` as the command (no arguments). Without a token, or with a wrong one, the server answers `401`.

## How it is built

- `crates/mcp` (`aitrack-mcp`): the server on [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) 3.11 (Apache-2.0, pinned exactly), on its own thread and Tokio runtime; the `aitrack-mcp` shim binary; the config snippets.
- The server only talks to the app through an `AppGateway` trait. In the app a channel carries each call to the UI thread (answered from the 33 ms housekeeping timer) and the server's thread waits for the answer, so network work never runs on the UI thread and nothing touches the audio thread. Tests fake the gateway.
- The tool list and execution are `aitrack_ai::llm::tools`, shared with the in-app assistant; `Broker` (in `crates/mcp`) turns the edits into a `History` proposal, and the app shows it with the assistant's proposal card.
- `AITRACK_DEV_VIEW=mcp` opens Preferences > MCP server with the server on, a made-up client and calls (nothing is started or saved).
