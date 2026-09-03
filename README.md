# UnpinBot

A Telegram bot to automatically unpin channel posts forwarded into connected discussion groups. Rewritten in Rust with [teloxide](https://github.com/teloxide/teloxide).

## Requirement

- Rust 1.82 or later (or just Docker)

## Configuration

All settings are passed via environment variables:

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `TELOXIDE_TOKEN` | yes | — | Bot token from @BotFather |
| `UNPINBOT_LANG` | no | `en` | UI language, `en` or `zh` |
| `UNPINBOT_STATE_PATH` | no | `pers_data/state.json` | Enabled-chats state file |
| `RUST_LOG` | no | `warn` | Log level (`info`, `debug`, ...) |
| `LOCAL_USER_ID` | no | `9001` | UID to run as inside the container (Docker only) |

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

## CI

GitHub Actions run format/clippy/tests on every PR and push to `master`, and build a Docker image to Docker Hub on pushes to `master` and `v*` tags.
