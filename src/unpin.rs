//! Auto-unpin handling, retry primitives, and permission predicates.

use std::future::Future;
use std::time::Duration;

use teloxide::prelude::*;
use teloxide::types::{
    Chat, ChatMember, ChatMemberKind, ChatMemberUpdated, ChatMigration, ChatPermissions, MessageId,
};
use teloxide::RequestError;

use crate::state::AppState;
use crate::Bot;

/// Maximum attempts for a Telegram call retried on network failures.
pub const MAX_ATTEMPTS: u32 = 3;

/// Backoff before each retry after the first attempt; the length is the
/// attempt budget, so a miscount cannot silently leave an entry unreachable.
const BACKOFF: [Duration; MAX_ATTEMPTS as usize - 1] =
    [Duration::from_millis(500), Duration::from_millis(1000)];

/// Runs `f` up to [`MAX_ATTEMPTS`] times, retrying `Network` errors with
/// backoff from [`BACKOFF`]. Any other error returns immediately.
///
/// `RetryAfter` (429) never reaches here: the `Throttle` adaptor this crate's
/// [`Bot`] is built with retries it internally, sleeping exactly as long as
/// Telegram demands and without an attempt cap. A 429 is the adaptor's
/// responsibility; this budget covers network failures only.
pub async fn with_retry<T, F, Fut>(f: F) -> Result<T, RequestError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, RequestError>>,
{
    let mut attempts = 0u32;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(err @ RequestError::Network(_)) => {
                if attempts + 1 >= MAX_ATTEMPTS {
                    return Err(err);
                }
                tokio::time::sleep(BACKOFF[attempts as usize]).await;
                attempts += 1;
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

/// Whether the bot itself can unpin in `chat`.
///
/// - Supergroup: bot must be an administrator with `can_pin_messages`.
/// - Basic group: an administrator bot does not carry `can_pin_messages`, so
///   the check falls back to the chat's default member permissions.
/// - Anything else: false.
pub fn bot_can_unpin(
    chat: &Chat,
    bot_member: &ChatMember,
    default_permissions: Option<ChatPermissions>,
) -> bool {
    if chat.is_supergroup() {
        matches!(&bot_member.kind,
            ChatMemberKind::Administrator(a) if a.can_pin_messages)
    } else if chat.is_group() {
        matches!(bot_member.kind, ChatMemberKind::Administrator(_))
            && default_permissions.is_some_and(|p| p.can_pin_messages())
    } else {
        false
    }
}

/// The default member permissions when `chat` is a basic group, `None`
/// otherwise: only a basic group keeps the pin right there, because an
/// administrator bot in one carries no `can_pin_messages` of its own.
pub async fn basic_group_permissions(
    bot: &Bot,
    chat: &Chat,
) -> Result<Option<ChatPermissions>, RequestError> {
    if !chat.is_group() {
        return Ok(None);
    }
    Ok(with_retry(|| bot.get_chat(chat.id).send())
        .await?
        .permissions())
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

/// The `(enabled, replacement)` chat ids a chat-migration message moves the
/// enabled set between, or `None` for a message that is not one.
///
/// A basic group upgraded to a supergroup gets a new id, so an entry keyed by
/// the old one would never match an update again: the bot would silently stop
/// unpinning there. Telegram reports the pair on the migration service
/// message, in both directions, depending on which side of the upgrade the
/// message was delivered on.
fn migration_pair(msg: &Message) -> Option<(ChatId, ChatId)> {
    match msg.chat_migration()? {
        // The service message in the upgraded supergroup names the old group.
        ChatMigration::From { chat_id } => Some((*chat_id, msg.chat.id)),
        // Legacy shape: the last message in the old group names the new one.
        ChatMigration::To { chat_id } => Some((msg.chat.id, *chat_id)),
    }
}

/// Keeps the enabled set following a basic group upgraded to a supergroup.
///
/// `unpin_with_retry` covers the remaining race, where the upgrade lands
/// between receiving an update and sending its unpin request to the old id.
pub async fn chat_migrated(msg: Message, state: AppState) -> ResponseResult<()> {
    // The dispatcher branch filters on the same condition; there is nothing
    // to do for a message that is not a migration.
    let Some((old, new)) = migration_pair(&msg) else {
        return Ok(());
    };
    match state.replace_and_save(old, new) {
        Ok(true) => log::info!("chat {old} upgraded to {new}; migrating enabled state"),
        Ok(false) => log::debug!("chat {old} was not enabled; nothing to migrate to {new}"),
        Err(e) => log::error!("failed to persist migration {old} -> {new}: {e}"),
    }
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
    // A transient failure must not flip state.
    let permissions = match basic_group_permissions(&bot, &upd.chat).await {
        Ok(permissions) => permissions,
        Err(e) => {
            log::warn!("get_chat failed for chat {chat_id} on rights change: {e}");
            return Ok(());
        }
    };
    if bot_can_unpin(&upd.chat, &upd.new_chat_member, permissions) {
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

/// What an `unpinChatMessage` failure means for [`unpin_with_retry`].
#[derive(Debug, PartialEq)]
enum UnpinFailure {
    /// The group was upgraded to a supergroup; retry against the new id.
    Migrated(ChatId),
    /// The bot lost the pin right; only an admin can fix this.
    NoRights,
    /// The bot is no longer in the chat.
    ChatGone,
    /// The goal already holds: nothing is pinned any more.
    AlreadyDone,
    /// Anything else, including network failures that outlived the retries.
    Fatal,
}

/// Classifies an unpin failure by the only thing the caller can do about it.
/// Telegram reports some of these ambiguously, so this mapping is the contract
/// [`unpin_with_retry`] relies on.
fn classify(err: &RequestError) -> UnpinFailure {
    match err {
        RequestError::MigrateToChatId(new_id) => UnpinFailure::Migrated(*new_id),
        RequestError::Api(teloxide::ApiError::NotEnoughRightsToManagePins)
        | RequestError::Api(teloxide::ApiError::NotEnoughRightsToPinMessage) => {
            UnpinFailure::NoRights
        }
        RequestError::Api(teloxide::ApiError::ChatNotFound) => UnpinFailure::ChatGone,
        // Nothing left to unpin: see `nothing_to_unpin`.
        RequestError::Api(err) if nothing_to_unpin(err) => UnpinFailure::AlreadyDone,
        _ => UnpinFailure::Fatal,
    }
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
            Err(err) => match classify(&err) {
                UnpinFailure::Migrated(new_id) => {
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
                        Err(e) => {
                            log::error!("failed to persist migration {chat_id} -> {new_id}: {e}")
                        }
                    }
                    migrated = true;
                    target = new_id;
                }
                UnpinFailure::NoRights => {
                    log::warn!(
                        "bot lacks pin rights in chat {chat_id}; re-run /enable after granting them"
                    );
                    return;
                }
                UnpinFailure::ChatGone => {
                    log::warn!("chat {chat_id} not found while unpinning");
                    return;
                }
                UnpinFailure::AlreadyDone => {
                    log::debug!("message {message_id} in chat {chat_id} is not pinned");
                    return;
                }
                UnpinFailure::Fatal => {
                    log::error!("unpin failed in chat {chat_id}: {err}");
                    return;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::EnabledChats;
    use std::sync::atomic::{AtomicU32, Ordering};
    use teloxide::types::{ChatMemberKind, ChatMemberStatus, User};

    /// A genuine `reqwest::Error`: `RequestError::Network` accepts nothing
    /// else. Any `reqwest::Error` triggers the same retry path — the logic only
    /// inspects the variant — and a malformed URL yields one without touching
    /// the network. Built once per test so the retry loop does no I/O and its
    /// timing stays exact.
    async fn network_error() -> std::sync::Arc<reqwest::Error> {
        let err = reqwest::Client::new()
            .get("not-a-url")
            .send()
            .await
            .expect_err("a relative URL must fail");
        std::sync::Arc::new(err)
    }

    #[tokio::test(start_paused = true)]
    async fn retries_transient_failures_up_to_the_attempt_budget() {
        let error = network_error().await;
        let calls = AtomicU32::new(0);
        let start = tokio::time::Instant::now();
        let result = with_retry(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            let error = error.clone();
            async move { Err::<(), _>(RequestError::Network(error)) }
        })
        .await;
        assert!(matches!(result, Err(RequestError::Network(_))));
        assert_eq!(calls.load(Ordering::SeqCst), MAX_ATTEMPTS);
        // Every attempt but the last waits out one backoff slot.
        assert_eq!(start.elapsed(), BACKOFF.iter().sum::<Duration>());
    }

    #[tokio::test(start_paused = true)]
    async fn stops_on_success_and_on_permanent_errors() {
        let error = network_error().await;
        let calls = AtomicU32::new(0);
        let start = tokio::time::Instant::now();
        let result = with_retry(|| {
            let attempt = calls.fetch_add(1, Ordering::SeqCst);
            let error = error.clone();
            async move {
                if attempt == 0 {
                    Err(RequestError::Network(error))
                } else {
                    Ok(attempt)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(start.elapsed(), BACKOFF[0]);

        let calls = AtomicU32::new(0);
        let result = with_retry(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(RequestError::Api(teloxide::ApiError::ChatNotFound)) }
        })
        .await;
        assert!(matches!(
            result,
            Err(RequestError::Api(teloxide::ApiError::ChatNotFound))
        ));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "permanent errors must not be retried"
        );
    }

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

    const OLD_CHAT: i64 = -599075523;
    const NEW_CHAT: i64 = -1001555296434;

    /// Parses a `Message` fixture; `serde_json` deserializes teloxide's
    /// message types, which is the only way to build one without a network.
    fn message(json: &str) -> Message {
        serde_json::from_str(json).expect("message fixture")
    }

    /// The migration service message Telegram delivers in the upgraded
    /// supergroup, whose `migrate_from_chat_id` names the old group.
    fn upgrade_message() -> Message {
        message(&format!(
            r#"{{"chat":{{"id":{NEW_CHAT},"title":"test","type":"supergroup"}},
                "date":1629404938,
                "from":{{"first_name":"n","id":729497414,"is_bot":true,"username":"unpinbot"}},
                "message_id":1,"migrate_from_chat_id":{OLD_CHAT}}}"#
        ))
    }

    #[test]
    fn migration_messages_name_the_old_and_the_new_chat() {
        assert_eq!(
            migration_pair(&upgrade_message()),
            Some((ChatId(OLD_CHAT), ChatId(NEW_CHAT)))
        );

        // Legacy shape: the last message in the old group names the new one.
        let legacy = message(&format!(
            r#"{{"chat":{{"id":{OLD_CHAT},"title":"test","type":"group"}},
                "date":1629404938,
                "from":{{"first_name":"n","id":729497414,"is_bot":true,"username":"unpinbot"}},
                "message_id":2,"migrate_to_chat_id":{NEW_CHAT}}}"#
        ));
        assert_eq!(
            migration_pair(&legacy),
            Some((ChatId(OLD_CHAT), ChatId(NEW_CHAT)))
        );

        // An ordinary message carries no migration.
        let plain = message(&format!(
            r#"{{"chat":{{"id":{OLD_CHAT},"title":"test","type":"group"}},
                "date":1,"message_id":3,"text":"hi"}}"#
        ));
        assert_eq!(migration_pair(&plain), None);
    }

    #[tokio::test]
    async fn upgrade_message_moves_the_enabled_entry_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, format!(r#"{{"enabled_chats":[{OLD_CHAT}]}}"#)).unwrap();
        let state = AppState::new(EnabledChats::load(&path).expect("state loads"));

        chat_migrated(upgrade_message(), state).await.unwrap();

        let reloaded = EnabledChats::load(&path).expect("state reloads");
        assert!(
            reloaded.contains(ChatId(NEW_CHAT)),
            "upgraded chat is enabled"
        );
        assert!(!reloaded.contains(ChatId(OLD_CHAT)), "old chat id is gone");
    }

    #[test]
    fn unpin_failures_are_classified_by_what_the_caller_can_do() {
        use teloxide::ApiError;
        assert_eq!(
            classify(&RequestError::MigrateToChatId(ChatId(-1001234567890))),
            UnpinFailure::Migrated(ChatId(-1001234567890))
        );
        for api in [
            ApiError::NotEnoughRightsToManagePins,
            ApiError::NotEnoughRightsToPinMessage,
        ] {
            assert_eq!(classify(&RequestError::Api(api)), UnpinFailure::NoRights);
        }
        assert_eq!(
            classify(&RequestError::Api(ApiError::ChatNotFound)),
            UnpinFailure::ChatGone
        );
        // The two shapes of "there is nothing pinned any more".
        assert_eq!(
            classify(&RequestError::Api(ApiError::MessageIdInvalid)),
            UnpinFailure::AlreadyDone
        );
        assert_eq!(
            classify(&RequestError::Api(ApiError::Unknown(
                "Bad Request: message to unpin not found".to_owned()
            ))),
            UnpinFailure::AlreadyDone
        );
        // An unrelated failure must not pass for a completed unpin.
        assert_eq!(
            classify(&RequestError::Api(ApiError::Unknown(
                "Bad Request: nope".to_owned()
            ))),
            UnpinFailure::Fatal
        );
    }

    #[test]
    fn privileged_members() {
        assert!(is_privileged(&member(owner())));
        assert!(is_privileged(&member(admin(true))));
        assert!(is_privileged(&member(admin(false))));
        assert!(!is_privileged(&member(regular())));
        assert!(!is_privileged(&member(ChatMemberKind::Left)));
    }

    /// A chat parsed from the wire format: these structs have no public
    /// constructor and far more fields than the predicates below look at.
    fn chat(kind: &str) -> Chat {
        let json = if kind == "private" {
            r#"{"id":42,"type":"private","first_name":"Test"}"#.to_owned()
        } else {
            format!(r#"{{"id":-1001234567890,"title":"Test","type":"{kind}"}}"#)
        };
        serde_json::from_str(&json).expect("chat fixture")
    }

    #[test]
    fn supergroup_requires_admin_pin_right() {
        let supergroup = chat("supergroup");
        let bot_member = member(admin(true));
        assert!(bot_can_unpin(&supergroup, &bot_member, None));
        let bot_member = member(admin(false));
        assert!(!bot_can_unpin(&supergroup, &bot_member, None));
        let bot_member = member(regular());
        assert!(!bot_can_unpin(&supergroup, &bot_member, None));
    }

    #[test]
    fn group_requires_default_pin_permission() {
        let group = chat("group");
        let bot_member = member(admin(false)); // basic-group admins carry no can_pin field
        assert!(bot_can_unpin(&group, &bot_member, pin_permissions(true)));
        assert!(!bot_can_unpin(&group, &bot_member, pin_permissions(false)));
        assert!(!bot_can_unpin(&group, &bot_member, None));
    }

    #[test]
    fn other_chat_types_are_never_unpinnable() {
        let bot_member = member(admin(true));
        assert!(!bot_can_unpin(
            &chat("private"),
            &bot_member,
            pin_permissions(true)
        ));
        assert!(!bot_can_unpin(
            &chat("channel"),
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
