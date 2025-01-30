use teloxide::dispatching::ShutdownToken;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::database::DatabaseClient;
use crate::Bot;

#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "lowercase",
    description = "Diese Befehle werden unterstützt:"
)]
enum Command {
    #[command(
        description = "für Benachrichtigungen registrieren.",
        parse_with = "default"
    )]
    Start(String),
    #[command(description = "Benachrichtigungen abbestellen.")]
    Stop,
    #[command(description = "zeige diesen Text.")]
    Help,
}

async fn handle_message(
    bot: Bot,
    msg: Message,
    cmd: Command,
    mut db: DatabaseClient,
) -> ResponseResult<()> {
    match cmd {
        Command::Start(gremium) => {
            let reply = if gremium.len() >= 256 {
                "Name des Gremiums zu lang!"
            } else {
                match db.register_chat(msg.chat.id, &gremium).await {
                    Ok(true) => {
                        log::info!("Chat {} registered: {gremium}", msg.chat.id);
                        "Du hast dich erfolgreich für Benachrichtigungen registriert."
                    }
                    Ok(false) => "Du bist bereits für Benachrichtigungen registriert.",
                    Err(e) => {
                        log::error!("Database error: {e}");
                        "Ein interner Fehler ist aufgetreten :(("
                    }
                }
            };

            bot.send_message(msg.chat.id, reply).await?;
        }
        Command::Stop => {
            let reply = match db.unregister_chat(msg.chat.id).await {
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
    mut db: DatabaseClient,
) -> ResponseResult<()> {
    if update.new_chat_member.can_post_messages() {
        match db.register_chat(update.chat.id, "").await {
            Ok(_) => log::info!("Added channel \"{}\"", update.chat.title().unwrap_or("")),
            Err(e) => log::error!(
                "Adding channel \"{}\" failed: {e}",
                update.chat.title().unwrap_or("<unknown>")
            ),
        }
    } else {
        match db.unregister_chat(update.chat.id).await {
            Ok(_) => log::info!("Removed channel \"{}\"", update.chat.title().unwrap_or("")),
            Err(e) => log::error!(
                "Removing channel \"{}\" failed: {e}",
                update.chat.title().unwrap_or("<unknown>")
            ),
        }
    }

    Ok(())
}

fn create(
    bot: Bot,
    db: DatabaseClient,
) -> Dispatcher<Bot, teloxide::RequestError, teloxide::dispatching::DefaultKey> {
    let handler = dptree::entry()
        .inspect(|u: Update| log::info!("{u:#?}"))
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
        .dependencies(dptree::deps![db])
        .error_handler(LoggingErrorHandler::with_custom_text(
            "An error has occurred in the dispatcher",
        ))
        .default_handler(|_| async {})
        .build()
}

pub struct DispatcherTask {
    token: Option<ShutdownToken>,
}

impl DispatcherTask {
    /// Creates a dispatcher to handle the bot's incoming messages.
    pub fn new(bot: Bot, db: DatabaseClient) -> Self {
        let mut dispatcher = create(bot, db);
        let token = dispatcher.shutdown_token();
        tokio::spawn(async move { dispatcher.dispatch().await });

        Self { token: Some(token) }
    }

    /// Does nothing but simplifies control flow in main function
    pub fn do_nothing() -> Self {
        Self { token: None }
    }

    pub async fn shutdown(self) {
        if let Some(token) = self.token {
            if let Ok(f) = token.shutdown() {
                f.await
            }
        }
    }
}
