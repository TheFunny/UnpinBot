# UnpinBot

A Telegram bot to automatically unpin channel posts forwarded into connected discussion groups. Rewritten in Rust with [teloxide](https://github.com/teloxide/teloxide).

## Requirement

- A recent stable Rust toolchain (or just Docker)

## Configuration

All settings are passed via environment variables:

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `TELOXIDE_TOKEN` | yes | — | Bot token from @BotFather |
| `TELOXIDE_PROXY` | no | — | Proxy for all Telegram requests, e.g. `socks5://127.0.0.1:1080` |
| `UNPINBOT_STATE_PATH` | no | `pers_data/state.json` | Enabled-chats state file |
| `RUST_LOG` | no | `info` | Log level (`warn` for quiet, `debug` for detail) |
| `LOCAL_USER_ID` | no | `9001` | UID to run as inside the container (Docker only; numeric, non-zero) |

## Deployment

### Docker Compose

0. Install `Docker`.
1. `cd` to the cloned directory.
2. Create a `.env` file next to `docker-compose.yml` containing your token:

   ```
   TELOXIDE_TOKEN=123456:ABC-DEF...
   ```

   (or set `TELOXIDE_TOKEN` directly in the `environment` section of `docker-compose.yml`).
3. If you run Docker as a non-root user on Linux, you may need to change `LOCAL_USER_ID` in `docker-compose.yml` to your own user id (check with `id -u`). This solves file permission issues on the `pers_data` volume.
4. Run `docker compose up -d`, use `docker logs unpin` to check for errors.
5. To stop the container, run `docker compose down` in the cloned directory.

### Local

0. Install Rust.
1. `cd` to the cloned directory.
2. Set the token and run:

   ```
   TELOXIDE_TOKEN=123456:ABC-DEF... cargo run --release
   ```

On Windows (PowerShell):

```powershell
$env:TELOXIDE_TOKEN = '123456:ABC-DEF...'
cargo run --release
```

## Usage

- `/enable` — enable auto-unpin in the current group (administrator only; the bot needs the pin-messages permission)
- `/disable` — disable auto-unpin
- `/start`, `/help` — about and help

Enabled chats are persisted in `pers_data/state.json` and survive restarts. If you are upgrading from an old Python-based release, the old state cannot be migrated — run `/enable` again in each group.

If the bot loses the pin-messages permission or is removed from the group, it disables auto-unpin there and says so in the group (when it still can). Grant the permission again and run `/enable` — no need to touch the state file by hand.

When a basic group is upgraded to a supergroup, the bot moves its enabled entry to the new chat id on its own and keeps unpinning without a new `/enable`.

The bot follows each sender's Telegram client language automatically (English and Chinese; English is the fallback). Command menus and the bot description match the client language too — no configuration needed.

## CI

Every PR and push to `master` runs format, clippy, the test suite, a release-profile build and a `cargo audit` gate. Pull requests that touch anything the image depends on also build the Docker image without pushing it; pushes to `master` and `v*` tags publish it to Docker Hub (`latest` follows both).

Releases are tagged from `master` — the tag must match the `version` in `Cargo.toml`, and the tag run fails otherwise. Dependabot keeps the dependencies and the pinned workflow actions current.
