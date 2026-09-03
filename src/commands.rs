//! `/start` `/help` `/enable` `/disable` command handlers.

use teloxide::prelude::*;
use teloxide::types::{ChatAction, ChatType, ParseMode};
use teloxide::utils::command::BotCommands;

/// Maps a chat's public kind to the wire `ChatType` used by the predicate.
fn chat_type_of(chat: &teloxide::types::Chat) -> ChatType {
    match &chat.kind {
        teloxide::types::ChatKind::Public(p) => match p.kind {
            teloxide::types::PublicChatKind::Group => ChatType::Group,
            teloxide::types::PublicChatKind::Supergroup(_) => ChatType::Supergroup,
            teloxide::types::PublicChatKind::Channel(_) => ChatType::Channel,
        },
        teloxide::types::ChatKind::Private(_) => ChatType::Private,
    }
}

use crate::i18n::Lang;
use crate::state::AppState;
use crate::unpin::{bot_can_unpin, is_privileged, with_retry};
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
    bot.send_message(msg.chat.id, text)
        .parse_mode(ParseMode::Html)
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
    if !matches!(
        chat_type_of(&msg.chat),
        ChatType::Group | ChatType::Supergroup
    ) {
        reply(bot, msg, &lang.error.not_group).await?;
        return Ok(false);
    }
    Ok(true)
}

/// Rejects non-admin callers. `None` sender (anonymous group admin sends
/// appear without a `from` user) is treated as not-admin.
async fn ensure_caller_admin(bot: &Bot, msg: &Message, lang: &Lang) -> ResponseResult<bool> {
    let Some(from) = msg.from.as_ref() else {
        reply(bot, msg, &lang.error.not_admin).await?;
        return Ok(false);
    };
    match with_retry(|| bot.get_chat_member(msg.chat.id, from.id).send()).await {
        Ok(member) if is_privileged(&member) => Ok(true),
        Ok(_) => {
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

pub async fn start(bot: Bot, msg: Message, lang: Lang) -> ResponseResult<()> {
    typing(&bot, &msg).await;
    reply(&bot, &msg, &lang.start).await
}

pub async fn help(bot: Bot, msg: Message, lang: Lang) -> ResponseResult<()> {
    typing(&bot, &msg).await;
    reply(&bot, &msg, &lang.help).await
}

pub async fn enable(bot: Bot, msg: Message, lang: Lang, state: AppState) -> ResponseResult<()> {
    typing(&bot, &msg).await;
    if !ensure_group(&bot, &msg, &lang).await? {
        return Ok(());
    }
    if !ensure_caller_admin(&bot, &msg, &lang).await? {
        return Ok(());
    }

    // Verify the bot itself may unpin here. Basic groups expose the pin right
    // via default chat permissions; supergroups via the bot admin rights.
    let default_permissions = if chat_type_of(&msg.chat) == ChatType::Group {
        match with_retry(|| bot.get_chat(msg.chat.id).send()).await {
            Ok(info) => info.permissions(),
            Err(e) => {
                log::error!("get_chat failed in chat {}: {e}", msg.chat.id);
                reply(&bot, &msg, &lang.error.retry_later).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    let bot_id = match with_retry(|| bot.get_me().send()).await {
        Ok(me) => me.user.id,
        Err(e) => {
            log::error!("get_me failed during enable: {e}");
            reply(&bot, &msg, &lang.error.retry_later).await?;
            return Ok(());
        }
    };
    let bot_member = match with_retry(|| bot.get_chat_member(msg.chat.id, bot_id).send()).await {
        Ok(m) => m,
        Err(e) => {
            log::error!("get_chat_member(bot) failed in chat {}: {e}", msg.chat.id);
            reply(&bot, &msg, &lang.error.retry_later).await?;
            return Ok(());
        }
    };
    if !bot_can_unpin(chat_type_of(&msg.chat), &bot_member, default_permissions) {
        reply(&bot, &msg, &lang.error.require_rights).await?;
        return Ok(());
    }

    match state.insert_and_save(msg.chat.id) {
        Ok(true) => reply(&bot, &msg, &lang.enable).await,
        Ok(false) => reply(&bot, &msg, &lang.error.already_enabled).await,
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

pub async fn disable(bot: Bot, msg: Message, lang: Lang, state: AppState) -> ResponseResult<()> {
    typing(&bot, &msg).await;
    if !ensure_group(&bot, &msg, &lang).await? {
        return Ok(());
    }
    if !ensure_caller_admin(&bot, &msg, &lang).await? {
        return Ok(());
    }

    match state.remove_and_save(msg.chat.id) {
        Ok(true) => reply(&bot, &msg, &lang.disable).await,
        Ok(false) => reply(&bot, &msg, &lang.error.already_disabled).await,
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
    lang: Lang,
    state: AppState,
) -> ResponseResult<()> {
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
