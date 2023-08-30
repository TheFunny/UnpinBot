import json
import logging
import asyncio
from telegram import Update, BotCommand, BotCommandScopeAllGroupChats, BotCommandScopeAllChatAdministrators, ChatAdministratorRights
from telegram.constants import ParseMode, ChatAction, ChatMemberStatus, ChatType
from telegram.ext import (
    Application,
    ApplicationBuilder,
    CommandHandler,
    MessageHandler,
    ContextTypes,
    Defaults,
    PicklePersistence,
    filters
)
from telegram.error import NetworkError

with open("config.json") as config_file:
    config = json.load(config_file)

with open("lang/{lang}.json".format(lang=config["lang"]), encoding='utf-8') as lang_file:
    text = json.load(lang_file)

logging.basicConfig(
    level=logging.WARNING,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


async def cmd_start(update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
    await update.effective_chat.send_action(ChatAction.TYPING)
    await update.effective_message.reply_text(text["start"])


async def cmd_help(update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
    await update.effective_chat.send_action(ChatAction.TYPING)
    await update.effective_message.reply_text(text["help"])


async def cmd_enable(update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
    chat = update.effective_chat
    await chat.send_action(ChatAction.TYPING)
    if chat.type not in [ChatType.GROUP, ChatType.SUPERGROUP]:
        await update.effective_message.reply_text(text["error"]["not_group"])
        return
    user = await chat.get_member(update.effective_user.id)
    if user.status not in [ChatMemberStatus.ADMINISTRATOR, ChatMemberStatus.OWNER]:
        await update.effective_message.reply_text(text["error"]["not_admin"])
        return
    user_bot = await context.bot.get_me()
    bot_chat_member = await chat.get_member(user_bot.id)
    if chat.type == ChatType.GROUP:
        if bot_chat_member.status != ChatMemberStatus.ADMINISTRATOR and bot_chat_member.can_pin_messages != True or chat.permissions.can_pin_messages == False:
            await update.effective_message.reply_text(text["error"]["require_rights"])
            return
    else:
        if bot_chat_member.status != ChatMemberStatus.ADMINISTRATOR and bot_chat_member.can_pin_messages != True:
            await update.effective_message.reply_text(text["error"]["require_rights"])
            return
    chat_dict = context.chat_data
    if chat_dict.get("enabled", None) in [None, False]:
        chat_dict["enabled"] = True
        await update.effective_message.reply_text(text["enable"])
    else:
        await update.effective_message.reply_text(text["error"]["already_enabled"])


async def cmd_disable(update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
    await update.effective_chat.send_action(ChatAction.TYPING)
    if update.effective_chat.type not in [ChatType.GROUP, ChatType.SUPERGROUP]:
        await update.effective_message.reply_text(text["error"]["not_group"])
        return
    user = await update.effective_chat.get_member(update.effective_user.id)
    if user.status not in [ChatMemberStatus.ADMINISTRATOR, ChatMemberStatus.OWNER]:
        await update.effective_message.reply_text(text["error"]["not_admin"])
        return
    chat_dict = context.chat_data
    if chat_dict.get("enabled", None):
        chat_dict["enabled"] = False
        await update.effective_message.reply_text(text["disable"])
    else:
        await update.effective_message.reply_text(text["error"]["already_disabled"])


async def unpin(update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
    if context.chat_data.get("enabled", False):
        await retry_on_error(update.effective_message.unpin, retry=3)


async def retry_on_error(func, wait=0.1, retry=0, *args, **kwargs): # unsure if this can solve the problem
    for i in range(retry):
        try:
            return await func(*args, **kwargs)
        except NetworkError:
            await asyncio.sleep(wait)
    return await func(*args, **kwargs)


async def post_init(application: Application) -> None:
    rights = ChatAdministratorRights(
        False, False, False, False, False, False, False, False, can_pin_messages=True)
    await application.bot.set_my_default_administrator_rights(rights)
    commands = [
        BotCommand('start', text["cmd"]["start"]),
        BotCommand('help', text["cmd"]["help"])
    ]
    commands_admin = [
        BotCommand('enable', text["cmd"]["enable"]),
        BotCommand('disable', text["cmd"]["disable"])
    ]
    await application.bot.set_my_commands(commands, scope=BotCommandScopeAllGroupChats)
    await application.bot.set_my_commands(commands + commands_admin, scope=BotCommandScopeAllChatAdministrators)
    await application.bot.set_my_description(text["description"])
    await application.bot.set_my_short_description(text["description"])


def main():
    defaults = Defaults(parse_mode=ParseMode.HTML,
                        disable_web_page_preview=True, allow_sending_without_reply=True)
    persistence = PicklePersistence(filepath='pers_data')
    application = ApplicationBuilder().token(config["token"]).defaults(defaults).post_init(
        post_init).persistence(persistence).read_timeout(10).get_updates_read_timeout(10).build()

    application.add_handler(CommandHandler('start', cmd_start))
    application.add_handler(CommandHandler('help', cmd_help))
    application.add_handler(CommandHandler('enable', cmd_enable))
    application.add_handler(CommandHandler('disable', cmd_disable))
    application.add_handler(MessageHandler(
        filters.IS_AUTOMATIC_FORWARD, unpin))

    application.run_polling()


if __name__ == "__main__":
    main()
