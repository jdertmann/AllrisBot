use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::database::RedisClient;
use crate::Bot;

#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "lowercase",
    description = "Diese Befehle werden unterstützt:"
)]
enum Command {
    #[command(description = "für Benachrichtigungen registrieren.")]
    Start,
    #[command(description = "Benachrichtigungen abbestellen.")]
    Stop,
    #[command(description = "zeige diesen Text.")]
    Help,
}

async fn handle_message(
    bot: Bot,
    msg: Message,
    cmd: Command,
    redis_client: RedisClient,
) -> ResponseResult<()> {
    match cmd {
        Command::Start => {
            let reply = match redis_client.register_chat(msg.chat.id).await {
                Ok(true) => {
                    log::info!("Chat {} registered", msg.chat.id);
                    "Du hast dich erfolgreich für Benachrichtigungen registriert."
                }
                Ok(false) => "Du bist bereits für Benachrichtigungen registriert.",
                Err(e) => {
                    log::error!("Database error: {e}");
                    "Ein interner Fehler ist aufgetreten :(("
                }
            };

            bot.send_message(msg.chat.id, reply).await?;
        }
        Command::Stop => {
            let reply = match redis_client.unregister_chat(msg.chat.id).await {
                Ok(true) => {
                    log::info!("Chat {} unregistered", msg.chat.id);
                    "Du hast die Benachrichtigungen abbestellt."
                }
                Ok(false) => "Du warst nicht für Benachrichtigungen registriert.",
                Err(e) => {
                    log::error!("Database error: {e}");
                    "Ein interner Fehler ist aufgetreten :(("
                }
            };

            bot.send_message(msg.chat.id, reply).await?;
        }
        Command::Help => {
            bot.send_message(msg.chat.id, Command::descriptions().to_string())
                .await?;
        }
    }
    Ok(())
}

fn is_channel_perm_update(update: ChatMemberUpdated) -> bool {
    if update.chat.is_channel() {
        update.old_chat_member.can_post_messages() != update.new_chat_member.can_post_messages()
    } else {
        false
    }
}

async fn handle_perm_update(
    update: ChatMemberUpdated,
    redis_client: RedisClient,
) -> ResponseResult<()> {
    if update.new_chat_member.can_post_messages() {
        match redis_client.register_chat(update.chat.id).await {
            Ok(_) => log::info!("Added channel \"{}\"", update.chat.title().unwrap_or("")),
            Err(e) => log::error!(
                "Adding channel \"{}\" failed: {e}",
                update.chat.title().unwrap_or("<unknown>")
            ),
        }
    } else {
        match redis_client.unregister_chat(update.chat.id).await {
            Ok(_) => log::info!("Removed channel \"{}\"", update.chat.title().unwrap_or("")),
            Err(e) => log::error!(
                "Removing channel \"{}\" failed: {e}",
                update.chat.title().unwrap_or("<unknown>")
            ),
        }
    }

    Ok(())
}

pub fn create(
    bot: Bot,
    redis_client: RedisClient,
) -> Dispatcher<Bot, teloxide::RequestError, teloxide::dispatching::DefaultKey> {
    let handler = dptree::entry()
        .inspect(|u: Update| log::debug!("{u:#?}"))
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint(handle_message),
        )
        .branch(
            Update::filter_my_chat_member()
                .filter(is_channel_perm_update)
                .endpoint(handle_perm_update),
        );

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![redis_client])
        .error_handler(LoggingErrorHandler::with_custom_text(
            "An error has occurred in the dispatcher",
        ))
        .default_handler(|_| async {})
        .build()
}
