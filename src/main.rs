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
use teloxide::types::{BotCommand, BotCommandScope, ChatAdministratorRights, ParseMode, UserId};
use teloxide::update_listeners::{polling_default, UpdateListener as _};

use config::Config;
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

/// Registers the bot's default admin rights, then the command menus and
/// descriptions for every embedded language plus the default variant.
/// Failures here are logged but not fatal: the core unpin loop does not
/// depend on them.
async fn setup_bot_profile(bot: &Bot, catalogs: &i18n::Catalogs) {
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

    // Register profile data once per embedded language (Telegram serves the
    // variant matching the user's client language automatically) and once
    // with no language code, which is the default shown to users whose
    // language has no dedicated variant.
    let targets = catalogs
        .all()
        .map(|(code, lang)| (Some(code), lang))
        .chain(std::iter::once((None, catalogs.resolve(None))));
    for (code, lang) in targets {
        register_profile(bot, code, lang).await;
    }
}

/// Registers the command menus and descriptions for one variant. `code` is
/// `None` for the default variant, where the Bot API wants `language_code`
/// omitted entirely.
async fn register_profile(bot: &Bot, code: Option<&str>, lang: &i18n::Lang) {
    let basic = vec![
        BotCommand::new("start", lang.cmd.start.clone()),
        BotCommand::new("help", lang.cmd.help.clone()),
    ];
    let mut admin_cmds = basic.clone();
    admin_cmds.extend([
        BotCommand::new("enable", lang.cmd.enable.clone()),
        BotCommand::new("disable", lang.cmd.disable.clone()),
    ]);

    // All four requests below share the same optional `language_code` setter.
    let group_cmds = bot
        .set_my_commands(basic.clone())
        .scope(BotCommandScope::AllGroupChats);
    let admin_scope_cmds = bot
        .set_my_commands(admin_cmds)
        .scope(BotCommandScope::AllChatAdministrators);
    let description = bot
        .set_my_description()
        .description(lang.description.clone());
    let short_description = bot
        .set_my_short_description()
        .short_description(lang.description.clone());
    let (group_cmds, admin_scope_cmds, description, short_description) = match code {
        Some(code) => (
            group_cmds.language_code(code),
            admin_scope_cmds.language_code(code),
            description.language_code(code),
            short_description.language_code(code),
        ),
        None => (group_cmds, admin_scope_cmds, description, short_description),
    };

    let label = code.unwrap_or("default");
    if let Err(e) = with_retry(|| group_cmds.clone().send()).await {
        log::error!("set_my_commands(AllGroupChats, {label}) failed: {e}");
    }
    if let Err(e) = with_retry(|| admin_scope_cmds.clone().send()).await {
        log::error!("set_my_commands(AllChatAdministrators, {label}) failed: {e}");
    }
    if let Err(e) = with_retry(|| description.clone().send()).await {
        log::error!("set_my_description({label}) failed: {e}");
    }
    if let Err(e) = with_retry(|| short_description.clone().send()).await {
        log::error!("set_my_short_description({label}) failed: {e}");
    }
}

fn build_handler(
    catalogs: &'static i18n::Catalogs,
) -> dptree::Handler<'static, ResponseResult<()>, teloxide::dispatching::DpHandlerDescription> {
    // Commands: `Update::filter_message()` injects `Message`, resolve the
    // sender's language into the chain (overwriting nothing: `Lang` is not a
    // static dependency), then `filter_command` consumes the `Message`
    // (official teloxide pattern).
    let command_branch = Update::filter_message()
        .filter_map(move |msg: Message| {
            Some(
                catalogs
                    .resolve(msg.from.as_ref().and_then(|u| u.language_code.as_deref()))
                    .clone(),
            )
        })
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

    // Rights changes: disabling the chat when the bot can no longer unpin
    // keeps the persisted set from going stale.
    let member_branch = Update::filter_my_chat_member().endpoint(unpin::my_chat_member);

    dptree::entry()
        .branch(unpin_branch)
        .branch(command_branch)
        .branch(member_branch)
}

/// Docker `stop` / `compose down` delivers SIGTERM, which teloxide's ctrlc
/// handler (SIGINT only) never sees: without this the container dies after
/// the 10s grace period with SIGKILL, potentially mid-`save()` of the state
/// file. Stopping the token unwinds the dispatcher exactly like Ctrl+C does.
/// Windows has no SIGTERM; ctrlc handles Ctrl+C there.
#[cfg(unix)]
fn spawn_sigterm_handler(stop_token: teloxide::stop::StopToken) {
    tokio::spawn(async move {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        sigterm.recv().await;
        log::info!("SIGTERM received, stopping the dispatcher");
        stop_token.stop();
    });
}

#[cfg(not(unix))]
fn spawn_sigterm_handler(_stop_token: teloxide::stop::StopToken) {}

/// Process-wide embedded catalogs, loaded on first use.
fn catalogs() -> &'static i18n::Catalogs {
    static CATALOGS: std::sync::LazyLock<Result<i18n::Catalogs, String>> =
        std::sync::LazyLock::new(i18n::Catalogs::load);
    match &*CATALOGS {
        Ok(c) => c,
        Err(e) => fatal(e.clone()),
    }
}

/// The bot's own user id, set from `get_me` during startup — before the
/// dispatcher runs, so handlers never need their own lookup.
static BOT_ID: std::sync::OnceLock<UserId> = std::sync::OnceLock::new();

/// The bot's own user id.
fn bot_id() -> UserId {
    *BOT_ID.get().expect("bot id is resolved during startup")
}

async fn run() {
    // Defaults to warn; RUST_LOG overrides (parse after the default so the
    // env directive replaces it — the reverse order silently swallows it).
    let mut logger = pretty_env_logger::formatted_builder();
    logger
        .filter_level(log::LevelFilter::Warn)
        .parse_default_env();
    logger.init();

    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => fatal(e),
    };
    let catalogs = catalogs();
    let chats = match EnabledChats::load(&cfg.state_path) {
        Ok(c) => c,
        Err(e) => fatal(e),
    };
    log::info!(
        "loaded {} enabled chats from {}",
        chats.len(),
        cfg.state_path.display()
    );
    let state = AppState::new(chats);
    let bot = make_bot(&cfg);

    // Fail fast on an unusable token or network instead of panicking later
    // inside the dispatcher's implicit get_me.
    let me = match bot.get_me().await {
        Ok(me) => me,
        Err(e) => fatal(format!("cannot reach Telegram with provided token: {e}")),
    };
    let _ = BOT_ID.set(me.user.id);
    log::info!(
        "bot @{} started",
        me.user.username.as_deref().unwrap_or("?")
    );

    setup_bot_profile(&bot, catalogs).await;

    // `polling_default` is the same 10s long-poll + delete-webhook listener
    // the code built by hand before; taking its stop token lets SIGTERM (and
    // the ctrlc handler) unwind the dispatcher gracefully.
    let mut listener = polling_default(bot.clone()).await;
    spawn_sigterm_handler(listener.stop_token());

    let handler = build_handler(catalogs);
    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .default_handler(|upd| async move {
            log::trace!("skipped update: {upd:?}");
        })
        .enable_ctrlc_handler()
        .build()
        .dispatch_with_listener(
            listener,
            teloxide::error_handlers::LoggingErrorHandler::with_custom_text(
                "an error from the update listener",
            ),
        )
        .await;
    log::info!("dispatcher stopped");
}

fn main() {
    // Basic groups and supergroups are the only relevant updates; keep the
    // unknown-lang and other config errors human-readable before tokio starts.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(run());
}
