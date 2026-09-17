//! Auto-unpin handling, retry primitives, and permission predicates.

use std::future::Future;
use std::time::Duration;

use teloxide::prelude::*;
use teloxide::types::{
    Chat, ChatKind, ChatMember, ChatMemberKind, ChatMemberUpdated, ChatPermissions, ChatType,
    MessageId, PublicChatKind,
};
use teloxide::RequestError;

use crate::state::AppState;
use crate::Bot;

/// Maximum attempts for a retried Telegram call.
pub const MAX_ATTEMPTS: u32 = 3;

/// Backoff before each retry after the first attempt; the length is the
/// attempt budget, so a miscount cannot silently leave an entry unreachable.
const BACKOFF: [Duration; MAX_ATTEMPTS as usize - 1] =
    [Duration::from_millis(500), Duration::from_millis(1000)];

/// Runs `f` up to [`MAX_ATTEMPTS`] times, retrying transient failures:
/// `Network` errors with backoff from [`BACKOFF`] and `RetryAfter` by sleeping
/// exactly as long as Telegram demands. Any other error returns immediately.
pub async fn with_retry<T, F, Fut>(f: F) -> Result<T, RequestError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, RequestError>>,
{
    let mut network_attempts = 0u32;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(RequestError::RetryAfter(secs)) => {
                if network_attempts + 1 >= MAX_ATTEMPTS {
                    return Err(RequestError::RetryAfter(secs));
                }
                tokio::time::sleep(secs.duration()).await;
                network_attempts += 1;
            }
            Err(err @ RequestError::Network(_)) => {
                if network_attempts + 1 >= MAX_ATTEMPTS {
                    return Err(err);
                }
                tokio::time::sleep(BACKOFF[network_attempts as usize]).await;
                network_attempts += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

/// Whether `member` may run privileged commands.
pub fn is_privileged(member: &ChatMember) -> bool {
    matches!(
        member.kind,
        ChatMemberKind::Owner(_) | ChatMemberKind::Administrator(_)
    )
}

/// Maps a chat's public kind to the wire [`ChatType`] used by the predicates.
pub fn chat_type_of(chat: &Chat) -> ChatType {
    match &chat.kind {
        ChatKind::Public(public) => match public.kind {
            PublicChatKind::Group => ChatType::Group,
            PublicChatKind::Supergroup(_) => ChatType::Supergroup,
            PublicChatKind::Channel(_) => ChatType::Channel,
        },
        ChatKind::Private(_) => ChatType::Private,
    }
}

/// Whether the bot itself can unpin in `chat_type`.
///
/// - Supergroup: bot must be an administrator with `can_pin_messages`.
/// - Basic group: an administrator bot does not carry `can_pin_messages`, so
///   the check falls back to the chat's default member permissions.
/// - Anything else: false.
pub fn bot_can_unpin(
    chat_type: ChatType,
    bot_member: &ChatMember,
    default_permissions: Option<ChatPermissions>,
) -> bool {
    match chat_type {
        ChatType::Supergroup => matches!(&bot_member.kind,
            ChatMemberKind::Administrator(a) if a.can_pin_messages),
        ChatType::Group => {
            matches!(bot_member.kind, ChatMemberKind::Administrator(_))
                && default_permissions.is_some_and(|p| p.can_pin_messages())
        }
        _ => false,
    }
}

/// Handler for automatically forwarded channel posts: unpins them in enabled
/// chats only.
pub async fn auto_unpin(bot: Bot, msg: Message, state: AppState) -> ResponseResult<()> {
    if !state.contains(msg.chat.id) {
        log::debug!("chat {} is not enabled; skipping unpin", msg.chat.id);
        return Ok(());
    }
    log::info!(
        "auto-forwarded channel post {} in chat {}; unpinning",
        msg.id,
        msg.chat.id
    );
    unpin_with_retry(&bot, msg.chat.id, msg.id, &state).await;
    Ok(())
}

/// Whether an `unpinChatMessage` failure means there is nothing to unpin: the
/// message is gone, or it was already unpinned — an admin got there first, or
/// a retried request that had actually succeeded. Either way the desired end
/// state is reached, so it must not be logged as an error.
///
/// Telegram has no dedicated error for the second case; it arrives as
/// [`ApiError::Unknown`] carrying the Bot API text `message to unpin not
/// found`.
fn nothing_to_unpin(err: &teloxide::ApiError) -> bool {
    match err {
        teloxide::ApiError::MessageIdInvalid => true,
        teloxide::ApiError::Unknown(text) => text.contains("message to unpin not found"),
        _ => false,
    }
}

/// Keeps enabled state honest when the bot's own rights change.
///
/// Telegram pushes `my_chat_member` on promotion, demotion, and removal.
/// `/enable` checks the rights once, so without this a revoked pin right
/// leaves the chat "enabled" forever: every later channel post fails and the
/// admin never learns why.
pub async fn my_chat_member(
    bot: Bot,
    upd: ChatMemberUpdated,
    state: AppState,
) -> ResponseResult<()> {
    let chat_id = upd.chat.id;
    if !state.contains(chat_id) {
        return Ok(());
    }
    let chat_type = chat_type_of(&upd.chat);
    // Basic groups carry no pin right on the bot's own membership; theirs
    // lives in the chat's default member permissions, exactly as in /enable.
    let default_permissions = if chat_type == ChatType::Group {
        match with_retry(|| bot.get_chat(chat_id).send()).await {
            Ok(info) => info.permissions(),
            // A transient failure must not flip state.
            Err(e) => {
                log::warn!("get_chat failed for chat {chat_id} on rights change: {e}");
                return Ok(());
            }
        }
    } else {
        None
    };
    if bot_can_unpin(chat_type, &upd.new_chat_member, default_permissions) {
        return Ok(());
    }

    match state.remove_and_save(chat_id) {
        Ok(true) => log::info!("chat {chat_id} disabled: the bot can no longer unpin there"),
        Ok(false) => return Ok(()),
        Err(e) => {
            log::error!("failed to persist disable for chat {chat_id}: {e}");
            return Ok(());
        }
    }

    // Best effort: after a kick the bot cannot deliver this at all.
    let lang = crate::catalogs().resolve(upd.from.language_code.as_deref());
    if let Err(e) = bot
        .send_message(chat_id, &lang.error.rights_revoked)
        .send()
        .await
    {
        log::debug!("could not announce disabled state in chat {chat_id}: {e}");
    }
    Ok(())
}

/// Unpins `message_id` with retry; migrates enabled-chat state when the group
/// was upgraded to a supergroup.
async fn unpin_with_retry(bot: &Bot, chat_id: ChatId, message_id: MessageId, state: &AppState) {
    let mut target = chat_id;
    let mut migrated = false;
    loop {
        match with_retry(|| bot.unpin_chat_message(target).message_id(message_id).send()).await {
            Ok(_) => {
                log::info!("unpinned message {message_id} in chat {chat_id}");
                return;
            }
            Err(RequestError::MigrateToChatId(new_id)) => {
                if migrated {
                    log::error!("chat {chat_id} migrated twice; giving up");
                    return;
                }
                log::info!("chat {chat_id} migrated to {new_id}; migrating state");
                match state.replace_and_save(chat_id, new_id) {
                    Ok(true) => log::info!("enabled state migrated {chat_id} -> {new_id}"),
                    Ok(false) => {
                        log::warn!("chat {chat_id} was not in enabled state during migration")
                    }
                    Err(e) => log::error!("failed to persist migration {chat_id} -> {new_id}: {e}"),
                }
                migrated = true;
                target = new_id;
            }
            Err(RequestError::Api(teloxide::ApiError::NotEnoughRightsToManagePins))
            | Err(RequestError::Api(teloxide::ApiError::NotEnoughRightsToPinMessage)) => {
                log::warn!(
                    "bot lacks pin rights in chat {chat_id}; re-run /enable after granting them"
                );
                return;
            }
            Err(RequestError::Api(teloxide::ApiError::ChatNotFound)) => {
                log::warn!("chat {chat_id} not found while unpinning");
                return;
            }
            // Nothing left to unpin: see `nothing_to_unpin`.
            Err(RequestError::Api(err)) if nothing_to_unpin(&err) => {
                log::debug!("message {message_id} in chat {chat_id} is not pinned");
                return;
            }
            Err(e) => {
                log::error!("unpin failed in chat {chat_id}: {e}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use teloxide::types::{ChatMemberKind, ChatMemberStatus, User};

    fn user(id: u64) -> User {
        User {
            id: UserId(id),
            is_bot: false,
            first_name: "Test".into(),
            last_name: None,
            username: None,
            language_code: None,
            is_premium: false,
            added_to_attachment_menu: false,
        }
    }

    fn member(kind: ChatMemberKind) -> ChatMember {
        ChatMember {
            user: user(1),
            kind,
        }
    }

    fn admin(can_pin: bool) -> ChatMemberKind {
        ChatMemberKind::Administrator(teloxide::types::Administrator {
            custom_title: None,
            is_anonymous: false,
            can_be_edited: false,
            can_manage_chat: false,
            can_change_info: false,
            can_post_messages: false,
            can_edit_messages: false,
            can_delete_messages: false,
            can_post_stories: false,
            can_edit_stories: false,
            can_delete_stories: false,
            can_manage_video_chats: false,
            can_invite_users: false,
            can_restrict_members: false,
            can_pin_messages: can_pin,
            can_manage_topics: false,
            can_promote_members: false,
        })
    }

    fn owner() -> ChatMemberKind {
        ChatMemberKind::Owner(teloxide::types::Owner {
            custom_title: None,
            is_anonymous: false,
        })
    }

    fn regular() -> ChatMemberKind {
        ChatMemberKind::Member(teloxide::types::Member { until_date: None })
    }

    fn pin_permissions(allow: bool) -> Option<ChatPermissions> {
        let mut p = ChatPermissions::empty();
        if allow {
            p |= ChatPermissions::PIN_MESSAGES;
        }
        Some(p)
    }

    #[test]
    fn unpin_failures_that_mean_already_unpinned() {
        use teloxide::ApiError;
        assert!(nothing_to_unpin(&ApiError::MessageIdInvalid));
        assert!(nothing_to_unpin(&ApiError::Unknown(
            "Bad Request: message to unpin not found".to_owned()
        )));
        assert!(!nothing_to_unpin(&ApiError::Unknown(
            "Bad Request: nope".to_owned()
        )));
        assert!(!nothing_to_unpin(&ApiError::ChatNotFound));
    }

    #[test]
    fn privileged_members() {
        assert!(is_privileged(&member(owner())));
        assert!(is_privileged(&member(admin(true))));
        assert!(is_privileged(&member(admin(false))));
        assert!(!is_privileged(&member(regular())));
        assert!(!is_privileged(&member(ChatMemberKind::Left)));
    }

    #[test]
    fn supergroup_requires_admin_pin_right() {
        let bot_member = member(admin(true));
        assert!(bot_can_unpin(ChatType::Supergroup, &bot_member, None));
        let bot_member = member(admin(false));
        assert!(!bot_can_unpin(ChatType::Supergroup, &bot_member, None));
        let bot_member = member(regular());
        assert!(!bot_can_unpin(ChatType::Supergroup, &bot_member, None));
    }

    #[test]
    fn group_requires_default_pin_permission() {
        let bot_member = member(admin(false)); // basic-group admins carry no can_pin field
        assert!(bot_can_unpin(
            ChatType::Group,
            &bot_member,
            pin_permissions(true)
        ));
        assert!(!bot_can_unpin(
            ChatType::Group,
            &bot_member,
            pin_permissions(false)
        ));
        assert!(!bot_can_unpin(ChatType::Group, &bot_member, None));
    }

    #[test]
    fn other_chat_types_are_never_unpinnable() {
        let bot_member = member(admin(true));
        assert!(!bot_can_unpin(
            ChatType::Private,
            &bot_member,
            pin_permissions(true)
        ));
        assert!(!bot_can_unpin(
            ChatType::Channel,
            &bot_member,
            pin_permissions(true)
        ));
        // Sanity: status mapping works as expected.
        assert_eq!(
            member(admin(true)).status(),
            ChatMemberStatus::Administrator
        );
    }
}
