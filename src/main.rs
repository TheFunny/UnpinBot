//! UnpinBot: automatically unpins channel posts auto-forwarded into the
//! connected discussion group.

mod commands;
mod config;
mod i18n;
mod state;
mod unpin;

use std::process::exit;

use teloxide::adaptors::{DefaultParseMode, Throttle};
use teloxide::prelude::*;
use teloxide::types::{BotCommand, BotCommandScope, ChatAdministratorRights, ParseMode};
use teloxide::update_listeners::Polling;

use config::Config;
use i18n::Lang;
use state::{AppState, EnabledChats};
use unpin::with_retry;

type Bot = Throttle<DefaultParseMode<teloxide::Bot>>;

fn make_bot(cfg: &Config) -> Bot {
    let mut builder = teloxide::net::default_reqwest_settings();
    if let Ok(proxy) = std::env::var("TELOXIDE_PROXY") {
        match reqwest::Proxy::all(&proxy) {
            Ok(p) => builder = builder.proxy(p),
            Err(e) => fatal(format!("invalid TELOXIDE_PROXY {proxy:?}: {e}")),
        }
    }
    let client = builder.build().expect("creating reqwest client");
    teloxide::Bot::with_client(cfg.token.clone(), client)
        .parse_mode(ParseMode::Html)
        .throttle(teloxide::adaptors::throttle::Limits {
            messages_per_sec_chat: 1,
            messages_per_min_chat: 20,
            messages_per_min_channel_or_supergroup: 20,
            messages_per_sec_overall: 25,
        })
}

fn fatal(msg: String) -> ! {
    eprintln!("unpinbot: {msg}");
    exit(1)
}

/// Registers the bot's default admin rights, commands, and description.
/// Failures here are logged but not fatal: the core unpin loop does not
/// depend on them.
async fn setup_bot_profile(bot: &Bot, lang: &Lang) {
    let rights = ChatAdministratorRights {
        is_anonymous: false,
        can_manage_chat: false,
        can_delete_messages: false,
        can_manage_video_chats: false,
        can_restrict_members: false,
        can_promote_members: false,
        can_change_info: false,
        can_invite_users: false,
        can_post_messages: None,
        can_edit_messages: None,
        can_pin_messages: Some(true),
        can_post_stories: None,
        can_edit_stories: None,
        can_manage_topics: Some(false),
        can_delete_stories: None,
    };
    if let Err(e) = with_retry(|| {
        bot.set_my_default_administrator_rights()
            .rights(rights.clone())
            .send()
    })
    .await
    {
        log::error!("set_my_default_administrator_rights failed: {e}");
    }

    let basic = vec![
        BotCommand::new("start", lang.cmd.start.clone()),
        BotCommand::new("help", lang.cmd.help.clone()),
    ];
    let admin = [
        BotCommand::new("enable", lang.cmd.enable.clone()),
        BotCommand::new("disable", lang.cmd.disable.clone()),
    ];

    if let Err(e) = with_retry(|| {
        bot.set_my_commands(basic.clone())
            .scope(BotCommandScope::AllGroupChats)
            .send()
    })
    .await
    {
        log::error!("set_my_commands(AllGroupChats) failed: {e}");
    }
    let mut admin_cmds = basic.clone();
    admin_cmds.extend_from_slice(&admin);
    if let Err(e) = with_retry(|| {
        bot.set_my_commands(admin_cmds.clone())
            .scope(BotCommandScope::AllChatAdministrators)
            .send()
    })
    .await
    {
        log::error!("set_my_commands(AllChatAdministrators) failed: {e}");
    }

    if let Err(e) = with_retry(|| {
        bot.set_my_description()
            .description(lang.description.clone())
            .send()
    })
    .await
    {
        log::error!("set_my_description failed: {e}");
    }
    if let Err(e) = with_retry(|| {
        bot.set_my_short_description()
            .short_description(lang.description.clone())
            .send()
    })
    .await
    {
        log::error!("set_my_short_description failed: {e}");
    }
}

fn build_handler(
) -> dptree::Handler<'static, ResponseResult<()>, teloxide::dispatching::DpHandlerDescription> {
    // Commands: `Update::filter_message()` injects `Message`, then
    // `filter_command` consumes it (official teloxide pattern).
    let command_branch = Update::filter_message()
        .filter_command::<commands::Command>()
        .endpoint(commands::route_command);

    // Auto-unpin: derive the `is_automatic_forward` decision from the raw
    // `Update` so no `Message` dependency is required in the predicate.
    let unpin_branch = Update::filter_message()
        .filter(|update: Update| {
            matches!(&update.kind, teloxide::types::UpdateKind::Message(m) if m.is_automatic_forward())
        })
        .filter_map(|update: Update| match update.kind {
            teloxide::types::UpdateKind::Message(m) => Some(m),
            _ => None,
        })
        .endpoint(unpin::auto_unpin);

    dptree::entry().branch(unpin_branch).branch(command_branch)
}

async fn run() {
    let mut builder = pretty_env_logger::formatted_builder();
    builder
        .parse_default_env()
        .filter_level(log::LevelFilter::Warn);
    builder.init();

    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => fatal(e),
    };
    let lang = match i18n::load(&cfg.lang) {
        Ok(l) => l,
        Err(e) => fatal(e),
    };
    let chats = match EnabledChats::load(&cfg.state_path) {
        Ok(c) => c,
        Err(e) => fatal(e),
    };
    let state = AppState::new(chats);
    let bot = make_bot(&cfg);

    // Fail fast on an unusable token or network instead of panicking later
    // inside the dispatcher's implicit get_me.
    let me = match bot.get_me().await {
        Ok(me) => me,
        Err(e) => fatal(format!("cannot reach Telegram with provided token: {e}")),
    };
    log::info!(
        "bot @{} started",
        me.user.username.as_deref().unwrap_or("?")
    );

    setup_bot_profile(&bot, &lang).await;

    #[allow(unused_mut)]
    let mut polling = Polling::builder(bot.clone())
        .timeout(std::time::Duration::from_secs(10))
        .delete_webhook()
        .await
        .build();

    // Docker `stop` / `compose down` sends SIGTERM, which teloxide's ctrlc
    // handler (SIGINT only) never sees: without this the container dies after
    // the 10s grace period with SIGKILL, potentially mid-`save()` of the
    // state file. Stopping the token unwinds the dispatcher exactly like
    // Ctrl+C does. Windows has no SIGTERM; ctrlc handles Ctrl+C there.
    #[cfg(unix)]
    {
        let stop_token = polling.stop_token();
        tokio::spawn(async move {
            let mut sigterm =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!("cannot install SIGTERM handler: {e}");
                        return;
                    }
                };
            sigterm.recv().await;
            log::info!("SIGTERM received, stopping the dispatcher");
            stop_token.stop();
        });
    }

    let handler = build_handler();
    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state, lang])
        .default_handler(|upd| async move {
            log::trace!("skipped update: {upd:?}");
        })
        .enable_ctrlc_handler()
        .build()
        .dispatch_with_listener(
            polling,
            teloxide::error_handlers::LoggingErrorHandler::with_custom_text(
                "an error from the update listener",
            ),
        )
        .await;
}

fn main() {
    // Basic groups and supergroups are the only relevant updates; keep the
    // unknown-lang and other config errors human-readable before tokio starts.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(run());
    // Silence unused warnings for types referenced only in tests.
    let _ = teloxide::types::ChatType::Private;
}
