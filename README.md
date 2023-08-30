# UnpinBot

A Telegram bot to automatically unpin channel post forwarded into connected discussion group

## Requirement

- Python 3.9 or later

## Deployment

### Preparation

1. Create a new bot using @BotFather. Save the token as it's required later.
2. Clone this responsory.
3. Copy `config.json.example` to `config.json` and edit it. Put you bot token from @BotFather into `token`. Change `lang` if you want.

### Docker Compose

0. Install `Docker`.
1. `cd` to cloned directory.
2. Change settings in `docker-compose.yml` if you want.
3. If you run git as a non-root user on Linux, you need to change environment variable in `docker-compose.yml`. Run `id -u` first, and you will get your current user id. Replace `'1000'` of  `LOCAL_USER_ID` in `docker-compose.yml` with your current user id. This solves file permission issue.
4. Run `docker compose up -d`, use `docker logs unpin` to check if any error occurs.
5. To stop the container, run `docker compose down` in the cloned directory.

### Local

0. Install `Python` with `pip`.
1. `cd` to cloned directory and run `pip install -r requirements.txt`.
2. Run `python main.py`.
