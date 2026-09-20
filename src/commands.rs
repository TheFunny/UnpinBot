//! `/start` `/help` `/enable` `/disable` command handlers.

use std::sync::Arc;

use teloxide::prelude::*;
use teloxide::types::{ChatAction, ReplyParameters};
use teloxide::utils::command::BotCommands;

use crate::i18n::Lang;
use crate::state::AppState;
use crate::unpin::{basic_group_permissions, bot_can_unpin, is_privileged, with_retry};
use crate::Bot;

#[derive(BotCommands, Clone, Debug, PartialEq)]
#[command(rename_rule = "lowercase", description = "UnpinBot commands")]
pub enum Command {
    Start,
    Help,
    Enable,
    Disable,
}

async fn reply(bot: &Bot, msg: &Message, text: &str) -> ResponseResult<()> {
    // Reply to the triggering message so the answer reads in context in busy
    // groups; `allow_sending_without_reply` keeps it working when that message
    // is gone by the time we answer.
    bot.send_message(msg.chat.id, text)
        .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply())
        .send()
        .await?;
    Ok(())
}

async fn typing(bot: &Bot, msg: &Message) {
    let _ = bot
        .send_chat_action(msg.chat.id, ChatAction::Typing)
        .send()
        .await;
}

/// Rejects non-group chats, returning true when the caller may proceed.
async fn ensure_group(bot: &Bot, msg: &Message, lang: &Lang) -> ResponseResult<bool> {
    if !msg.chat.is_group() && !msg.chat.is_supergroup() {
        reply(bot, msg, &lang.error.not_group).await?;
        return Ok(false);
    }
    Ok(true)
}

/// Rejects non-admin callers. `None` sender (anonymous group admin sends
/// appear without a `from` user) is treated as not-admin.
async fn ensure_caller_admin(bot: &Bot, msg: &Message, lang: &Lang) -> ResponseResult<bool> {
    // Anonymous group admins send as GroupAnonymousBot with `sender_chat`
    // set to the chat itself; there is no real user to look up.
    if msg
        .sender_chat
        .as_ref()
        .is_some_and(|c| c.id == msg.chat.id)
    {
        return Ok(true);
    }
    let Some(from) = msg.from.as_ref() else {
        reply(bot, msg, &lang.error.not_admin).await?;
        return Ok(false);
    };
    match with_retry(|| bot.get_chat_member(msg.chat.id, from.id).send()).await {
        Ok(member) if is_privileged(&member) => Ok(true),
        Ok(_) => {
            log::info!("user {} is not an admin in chat {}", from.id, msg.chat.id);
            reply(bot, msg, &lang.error.not_admin).await?;
            Ok(false)
        }
        Err(e) => {
            log::error!("get_chat_member failed in chat {}: {e}", msg.chat.id);
            reply(bot, msg, &lang.error.retry_later).await?;
            Ok(false)
        }
    }
}

pub async fn start(bot: Bot, msg: Message, lang: Arc<Lang>) -> ResponseResult<()> {
    typing(&bot, &msg).await;
    reply(&bot, &msg, &lang.start).await
}

pub async fn help(bot: Bot, msg: Message, lang: Arc<Lang>) -> ResponseResult<()> {
    typing(&bot, &msg).await;
    reply(&bot, &msg, &lang.help).await
}

pub async fn enable(
    bot: Bot,
    msg: Message,
    lang: Arc<Lang>,
    state: AppState,
) -> ResponseResult<()> {
    typing(&bot, &msg).await;
    if !ensure_group(&bot, &msg, &lang).await? {
        return Ok(());
    }
    if !ensure_caller_admin(&bot, &msg, &lang).await? {
        return Ok(());
    }

    // Verify the bot itself may unpin here. Basic groups expose the pin right
    // via default chat permissions; supergroups via the bot admin rights.
    let permissions = match basic_group_permissions(&bot, &msg.chat).await {
        Ok(permissions) => permissions,
        Err(e) => {
            log::error!("get_chat failed in chat {}: {e}", msg.chat.id);
            reply(&bot, &msg, &lang.error.retry_later).await?;
            return Ok(());
        }
    };
    let bot_id = crate::bot_id();
    let bot_member = match with_retry(|| bot.get_chat_member(msg.chat.id, bot_id).send()).await {
        Ok(m) => m,
        Err(e) => {
            log::error!("get_chat_member(bot) failed in chat {}: {e}", msg.chat.id);
            reply(&bot, &msg, &lang.error.retry_later).await?;
            return Ok(());
        }
    };
    if !bot_can_unpin(&msg.chat, &bot_member, permissions) {
        log::info!(
            "bot cannot unpin in chat {} (missing rights); /enable rejected",
            msg.chat.id
        );
        reply(&bot, &msg, &lang.error.require_rights).await?;
        return Ok(());
    }

    match state.insert_and_save(msg.chat.id) {
        Ok(true) => {
            log::info!("chat {} enabled", msg.chat.id);
            reply(&bot, &msg, &lang.enable).await
        }
        Ok(false) => {
            log::info!("chat {} already enabled", msg.chat.id);
            reply(&bot, &msg, &lang.error.already_enabled).await
        }
        Err(e) => {
            log::error!(
                "failed to persist enabled state for chat {}: {e}",
                msg.chat.id
            );
            reply(&bot, &msg, &lang.error.retry_later).await?;
            Ok(())
        }
    }
}

pub async fn disable(
    bot: Bot,
    msg: Message,
    lang: Arc<Lang>,
    state: AppState,
) -> ResponseResult<()> {
    typing(&bot, &msg).await;
    if !ensure_group(&bot, &msg, &lang).await? {
        return Ok(());
    }
    if !ensure_caller_admin(&bot, &msg, &lang).await? {
        return Ok(());
    }

    match state.remove_and_save(msg.chat.id) {
        Ok(true) => {
            log::info!("chat {} disabled", msg.chat.id);
            reply(&bot, &msg, &lang.disable).await
        }
        Ok(false) => {
            log::info!("chat {} already disabled", msg.chat.id);
            reply(&bot, &msg, &lang.error.already_disabled).await
        }
        Err(e) => {
            log::error!(
                "failed to persist disabled state for chat {}: {e}",
                msg.chat.id
            );
            reply(&bot, &msg, &lang.error.retry_later).await?;
            Ok(())
        }
    }
}

/// dptree endpoint dispatching a parsed command to its handler.
pub async fn route_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    lang: Arc<Lang>,
    state: AppState,
) -> ResponseResult<()> {
    let sender = msg
        .from
        .as_ref()
        .map_or_else(|| "<anon>".to_owned(), |u| u.id.0.to_string());
    log::info!(
        "command {:?} from user {sender} in chat {}",
        cmd,
        msg.chat.id
    );
    match cmd {
        Command::Start => start(bot, msg, lang).await,
        Command::Help => help(bot, msg, lang).await,
        Command::Enable => enable(bot, msg, lang, state).await,
        Command::Disable => disable(bot, msg, lang, state).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_command() {
        assert_eq!(
            Command::parse("/enable", "unpinbot").unwrap(),
            Command::Enable
        );
        assert_eq!(
            Command::parse("/start", "unpinbot").unwrap(),
            Command::Start
        );
        assert_eq!(Command::parse("/help", "unpinbot").unwrap(), Command::Help);
        assert_eq!(
            Command::parse("/disable", "unpinbot").unwrap(),
            Command::Disable
        );
    }

    #[test]
    fn parses_mention_command_for_this_bot() {
        assert_eq!(
            Command::parse("/enable@unpinbot", "unpinbot").unwrap(),
            Command::Enable
        );
    }

    #[test]
    fn rejects_mention_for_other_bot() {
        assert!(Command::parse("/enable@otherbot", "unpinbot").is_err());
    }

    #[test]
    fn rejects_unknown_command() {
        assert!(Command::parse("/foo", "unpinbot").is_err());
    }
}
