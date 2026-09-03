//! Auto-unpin handling, retry primitives, and permission predicates.

use std::future::Future;
use std::time::Duration;

use teloxide::prelude::*;
use teloxide::types::{ChatMember, ChatMemberKind, ChatPermissions, ChatType, MessageId};
use teloxide::RequestError;

use crate::state::AppState;
use crate::Bot;

/// Maximum attempts for a retried Telegram call.
pub const MAX_ATTEMPTS: u32 = 3;

const BACKOFF: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_millis(1000),
    Duration::from_millis(2000),
];

/// Runs `f` up to [`MAX_ATTEMPTS`] times, retrying transient failures:
/// `Network` errors with exponential backoff (0.5s/1s/2s) and `RetryAfter`
/// by sleeping exactly as long as Telegram demands. Any other error returns
/// immediately.
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
        return Ok(());
    }
    unpin_with_retry(&bot, msg.chat.id, msg.id, &state).await;
    Ok(())
}

/// Unpins `message_id` with retry; migrates enabled-chat state when the group
/// was upgraded to a supergroup.
async fn unpin_with_retry(bot: &Bot, chat_id: ChatId, message_id: MessageId, state: &AppState) {
    let mut target = chat_id;
    let mut migrated = false;
    loop {
        match with_retry(|| bot.unpin_chat_message(target).message_id(message_id).send()).await {
            Ok(_) => return,
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
