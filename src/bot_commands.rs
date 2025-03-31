use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use futures_util::future::BoxFuture;
use regex::Regex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use teloxide::dispatching::ShutdownToken;
use teloxide::dispatching::dialogue::Storage;
use teloxide::prelude::*;
use teloxide::types::{
    ButtonRequest, ChatAdministratorRights, KeyboardButton, KeyboardButtonRequestChat,
    KeyboardMarkup, Me, ReplyMarkup, RequestId,
};
use teloxide::utils::command::BotCommands;
use tokio::sync::Mutex;

use crate::admin::AdminToken;
use crate::allris::AllrisUrl;
use crate::database::{self, DatabaseConnection, SharedDatabaseConnection};
use crate::types::{Condition, Filter, Tag};
use crate::{Bot, allris};

#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "lowercase",
    description = "Diese Befehle werden unterstützt:"
)]
enum Command {
    #[command(description = "eine neue Filterregel hinzufügen.")]
    AddFilter,
    #[command(description = "bestehende Filterregeln anzeigen.")]
    ListFilters,
    #[command(description = "eine Filterregel löschen.")]
    DeleteFilter,
    #[command(description = "alle Filterregeln löschen.")]
    DeleteAllFilters,
    #[command(description = "Einstellungen für einen Channel bearbeiten")]
    ManageChannel,
    #[command(description = "zeige diesen Text.")]
    Help,
    #[command(hide)]
    Cancel,
    #[command(hide)]
    Admin(String),
    #[command(hide)]
    ForceUpdate,
    #[command(hide)]
    ScanDay(chrono::NaiveDate),
}

struct Context {
    allris_url: AllrisUrl,
    database: SharedDatabaseConnection,
    token: Option<Mutex<AdminToken>>,
}

impl<Dialogue> Storage<Dialogue> for Context
where
    Dialogue: Send + Sync + Serialize + DeserializeOwned + 'static,
{
    type Error = database::Error;

    fn remove_dialogue(
        self: Arc<Self>,
        chat_id: ChatId,
    ) -> BoxFuture<'static, Result<(), Self::Error>> {
        Box::pin(async move { self.database.remove_dialogue(chat_id.0).await })
    }

    fn update_dialogue(
        self: Arc<Self>,
        chat_id: ChatId,
        dialogue: Dialogue,
    ) -> BoxFuture<'static, Result<(), Self::Error>> {
        Box::pin(async move { self.database.update_dialogue(chat_id.0, &dialogue).await })
    }

    fn get_dialogue(
        self: Arc<Self>,
        chat_id: ChatId,
    ) -> BoxFuture<'static, Result<Option<Dialogue>, Self::Error>> {
        Box::pin(async move { self.database.get_dialogue(chat_id.0).await })
    }
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub enum State {
    #[default]
    Start,
    ReceiveChannelSelection {
        request_id: RequestId,
    },
    ReceivingTag {
        previous_conditions: Vec<Condition>,
    },
    ReceivingNegation {
        previous_conditions: Vec<Condition>,
        tag: Tag,
    },
    ReceivingPattern {
        previous_conditions: Vec<Condition>,
        tag: Tag,
        negation: bool,
    },
    DeletingFilter,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct StateWithChannel {
    channel: Option<ChatId>,
    state: State,
}

type FilterDialogue = Dialogue<StateWithChannel, Context>;
type HandlerResult = Result<(), HandlerError>;

fn tag_keyboard() -> ReplyMarkup {
    let mut keyboard = vec![vec![KeyboardButton {
        text: "Speichern".into(),
        request: None,
    }]];
    keyboard.extend(Tag::TAGS.chunks(2).map(|tags| {
        tags.iter()
            .map(|tag| KeyboardButton {
                text: tag.label().into(),
                request: None,
            })
            .collect()
    }));
    ReplyMarkup::Keyboard(KeyboardMarkup {
        keyboard,
        is_persistent: false,
        resize_keyboard: true,
        one_time_keyboard: true,
        input_field_placeholder: "Tag auswählen".into(),
        selective: false,
    })
}

fn negation_keyboard() -> ReplyMarkup {
    let keyboard = vec![
        vec![KeyboardButton::new("Wenn das Pattern zutrifft")],
        vec![KeyboardButton::new("Wenn das Pattern nicht zutrifft")],
    ];
    ReplyMarkup::Keyboard(KeyboardMarkup {
        keyboard,
        is_persistent: false,
        resize_keyboard: true,
        one_time_keyboard: true,
        input_field_placeholder: "Antwort auswählen".into(),
        selective: false,
    })
}

async fn handle_message(
    bot: Bot,
    me: Me,
    dialogue: FilterDialogue,
    state: StateWithChannel,
    msg: Message,
    context: Arc<Context>,
) -> HandlerResult {
    let bot2 = bot.clone();
    let chat_id = msg.chat.id;
    let channel = state.channel;

    let result = async {
        if let Some(new_chat_id) = msg.migrate_to_chat_id() {
            let old_chat_id = msg.chat.id;
            log::info!("Migrating chat {old_chat_id} to {new_chat_id}");
            context
                .database
                .migrate_chat(old_chat_id.0, new_chat_id.0)
                .await?;
            FilterDialogue::new(context, *new_chat_id)
                .update(state)
                .await?;
            dialogue.exit().await?;
            return Ok(());
        }

        let command = msg
            .text()
            .and_then(|text| Command::parse(text, me.username()).ok());

        if matches!(command, Some(Command::Cancel)) {
            let message = if matches!(state.state, State::Start) {
                "Kein Befehl aktiv ..."
            } else {
                "Befehl wurde abgebrochen!"
            };
            dialogue.reset().await?;
            bot.send_message(msg.chat.id, message).await?;
            return Ok(());
        }

        match state.state {
            State::Start => {
                if let Some(command) = command {
                    handle_command(&bot, &dialogue, &msg, &command, &context, channel).await?;
                }
            }
            State::ReceivingTag {
                previous_conditions,
            } => receive_tag(bot, context, dialogue, previous_conditions, msg, channel).await?,
            State::ReceivingNegation {
                previous_conditions,
                tag,
            } => receive_negation(bot, dialogue, previous_conditions, tag, msg, channel).await?,
            State::ReceivingPattern {
                previous_conditions,
                tag,
                negation,
            } => {
                receive_pattern(
                    bot,
                    dialogue,
                    previous_conditions,
                    tag,
                    negation,
                    msg,
                    channel,
                )
                .await?
            }
            State::DeletingFilter => {
                delete_filter(&bot, &dialogue, &msg, &context, channel).await?
            }
            State::ReceiveChannelSelection { request_id } => {
                receive_channel_selection(&bot, &dialogue, &msg, request_id).await?
            }
        };

        Ok(())
    }
    .await;

    if let Err(e) = result.as_ref() {
        if !matches!(e, HandlerError::Bot(_)) {
            bot2.send_message(chat_id, "Interner Fehler :((").await?;
        }
    }

    result
}

#[derive(Debug, thiserror::Error)]
enum HandlerError {
    #[error("Database error: {0}")]
    Database(#[from] database::Error),
    #[error("Storage error: {0}")]
    Storage(#[from] teloxide::dispatching::dialogue::InMemStorageError),
    #[error("Bot error: {0}")]
    Bot(#[from] teloxide::errors::RequestError),
}

async fn handle_command(
    bot: &Bot,
    dialogue: &FilterDialogue,
    msg: &Message,
    command: &Command,
    context: &Context,
    channel: Option<ChatId>,
) -> HandlerResult {
    match command {
        Command::AddFilter => add_filter(bot, dialogue, msg, channel).await?,
        Command::ListFilters => list_filters(bot, msg, context, dialogue, channel).await?,
        Command::DeleteFilter => start_delete_filter(bot, msg, context, dialogue, channel).await?,
        Command::DeleteAllFilters => {
            delete_all_filters(bot, msg, context, dialogue, channel).await?
        }
        Command::Help => help(bot, msg).await?,
        Command::Admin(token) => admin(bot, msg, context, token).await?,
        Command::Cancel => (),
        Command::ScanDay(date) => scan_day(bot, msg, context, date).await?,
        Command::ManageChannel => manage_channel(bot, msg, dialogue).await?,
        Command::ForceUpdate => force_update(bot, msg, context).await?,
    }

    Ok(())
}

macro_rules! check_admin_permission {
    ($db:expr,$chat:expr) => {
        if !$db.is_admin($chat.id.0).await? {
            let user = $chat.title().or_else(|| $chat.username());
            log::warn!(
                "User {} [{user:?}] tried to use command without permission!",
                $chat.id.0
            );
            return Ok(());
        }
    };
}

macro_rules! user_or_channel_checked {
    ($bot:expr, $user:expr, $channel:expr, $dialogue:expr) => {
        if let Some(channel) = $channel {
            if !$bot
                .get_chat_administrators(channel)
                .await?
                .iter()
                .any(|admin| admin.user.id == $user)
            {
                $bot.send_message(
                    $user,
                    "Du hast keine Admin-Rechte in diesem Channel, Abbruch!",
                )
                .await?;
                $dialogue.reset().await?;
                return Ok(());
            }
            channel
        } else {
            $user
        }
        .0
    };
}

async fn manage_channel(bot: &Bot, msg: &Message, dialogue: &FilterDialogue) -> HandlerResult {
    if !msg.chat.is_private() {
        return Ok(());
    }

    let request_id = RequestId(msg.id.0);
    let button = KeyboardButtonRequestChat::new(request_id, true)
        .user_administrator_rights(ChatAdministratorRights {
            is_anonymous: false,
            can_manage_chat: false,
            can_delete_messages: false,
            can_manage_video_chats: false,
            can_restrict_members: false,
            can_promote_members: false,
            can_change_info: false,
            can_invite_users: true,
            can_post_messages: Some(true),
            can_edit_messages: None,
            can_pin_messages: None,
            can_post_stories: None,
            can_edit_stories: None,
            can_delete_stories: None,
            can_manage_topics: None,
        })
        .bot_administrator_rights(ChatAdministratorRights {
            is_anonymous: false,
            can_manage_chat: false,
            can_delete_messages: false,
            can_manage_video_chats: false,
            can_restrict_members: false,
            can_promote_members: false,
            can_change_info: false,
            can_invite_users: false,
            can_post_messages: Some(true),
            can_edit_messages: None,
            can_pin_messages: None,
            can_post_stories: None,
            can_edit_stories: None,
            can_delete_stories: None,
            can_manage_topics: None,
        });
    // TODO (later version of teloxide): request_title = true

    let button =
        KeyboardButton::new("Channel auswählen").request(ButtonRequest::RequestChat(button));
    let keyboard = KeyboardMarkup::new(vec![vec![button]])
        .one_time_keyboard()
        .resize_keyboard();

    dialogue
        .update(StateWithChannel {
            channel: None,
            state: State::ReceiveChannelSelection { request_id },
        })
        .await?;
    bot.send_message(msg.chat.id, "Bitte wähle einen Channel aus.")
        .reply_markup(keyboard)
        .await?;

    Ok(())
}

async fn scan_day(bot: &Bot, msg: &Message, context: &Context, date: &NaiveDate) -> HandlerResult {
    check_admin_permission!(context.database, msg.chat);

    let mut db = context.database.get_dedicated().await?;

    let message = match allris::scan_day(&context.allris_url, &mut db, *date).await {
        Ok(()) => "OK!",
        Err(e) => {
            log::error!("Update failed: {e}");
            "Ein Fehler ist aufgetreten. Schau im Log nach!"
        }
    };

    bot.send_message(msg.chat.id, message).await?;

    Ok(())
}

async fn force_update(bot: &Bot, msg: &Message, context: &Context) -> HandlerResult {
    check_admin_permission!(context.database, msg.chat);

    let mut db = context.database.get_dedicated().await?;
    let message = match allris::do_update(&context.allris_url, &mut db).await {
        Ok(()) => "OK!",
        Err(e) => {
            log::error!("Update failed: {e}");
            "Ein Fehler ist aufgetreten. Schau im Log nach!"
        }
    };

    bot.send_message(msg.chat.id, message).await?;

    Ok(())
}

async fn delete_filter(
    bot: &Bot,
    dialogue: &FilterDialogue,
    msg: &Message,
    context: &Context,
    channel: Option<ChatId>,
) -> HandlerResult {
    let chat_id = user_or_channel_checked!(bot, msg.chat.id, channel, dialogue);

    dialogue.exit().await?;

    let index = msg
        .text()
        .and_then(|text| text.strip_prefix("Filter "))
        .and_then(|text| text.parse().ok())
        .and_then(|x: usize| x.checked_sub(1));

    let index = match index {
        Some(x) => x,
        _ => {
            let message = "Ungültige Eingabe, Abbruch!";
            bot.send_message(msg.chat.id, message).await?;
            return Ok(());
        }
    };

    let removed = context
        .database
        .update_filter(chat_id, &|filters| {
            let valid = index < filters.len();
            filters.remove(index);
            valid
        })
        .await?;

    let message = if removed {
        "Alles klar, Filter wurde entfernt!"
    } else {
        "Diesen Filter scheint es nicht zu geben :/"
    };

    bot.send_message(msg.chat.id, message).await?;

    Ok(())
}

async fn list_filters(
    bot: &Bot,
    msg: &Message,
    context: &Context,
    dialogue: &FilterDialogue,
    channel: Option<ChatId>,
) -> HandlerResult {
    use std::fmt::Write;

    let chat_id = user_or_channel_checked!(bot, msg.chat.id, channel, dialogue);

    let filters = context.database.get_filters(chat_id).await?;
    let mut response = String::new();

    if filters.is_empty() {
        response += "Zur Zeit sind keine Filter aktiv.";
    } else {
        response += "Zur Zeit sind folgende Filter aktiv:\n\n";
        for (i, filter) in filters.iter().enumerate() {
            writeln!(response, "Filter {}:\n{filter}", i + 1).unwrap();
        }
    }

    bot.send_message(msg.chat.id, response).await?;

    Ok(())
}

async fn start_delete_filter(
    bot: &Bot,
    msg: &Message,
    context: &Context,
    dialogue: &FilterDialogue,
    channel: Option<ChatId>,
) -> HandlerResult {
    use std::fmt::Write;

    let chat_id = user_or_channel_checked!(bot, msg.chat.id, channel, dialogue);
    let filters = context.database.get_filters(chat_id).await?;
    let mut response = String::new();

    if filters.is_empty() {
        response += "Zur Zeit sind keine Filter aktiv.";
    } else {
        dialogue
            .update(StateWithChannel {
                state: State::DeletingFilter,
                channel,
            })
            .await?;

        response += "Wähle einen der folgenden Filter zum Löschen:\n\n";
        for (i, filter) in filters.iter().enumerate() {
            writeln!(response, "Filter {}:\n{filter}", i + 1).unwrap();
        }
    }

    let buttons: Vec<_> = (1..=filters.len())
        .map(|i| KeyboardButton::new(format!("Filter {i}")))
        .collect();
    let keyboard: Vec<_> = buttons.chunks(3).map(Vec::from).collect();

    bot.send_message(msg.chat.id, response)
        .reply_markup(ReplyMarkup::Keyboard(
            KeyboardMarkup::new(keyboard).one_time_keyboard(),
        ))
        .await?;

    Ok(())
}

async fn add_filter(
    bot: &Bot,
    dialogue: &FilterDialogue,
    msg: &Message,
    channel: Option<ChatId>,
) -> HandlerResult {
    user_or_channel_checked!(bot, msg.chat.id, channel, dialogue);

    dialogue
        .update(StateWithChannel {
            channel,
            state: State::ReceivingTag {
                previous_conditions: vec![],
            },
        })
        .await?;
    let text = "Lass uns einen Filter einrichten! Du kannst den Filter auch sofort speichern, um alle Vorlagen einzuschließen.\n\nBitte gib den ersten Tag ein:";
    bot.send_message(msg.chat.id, text)
        .reply_markup(tag_keyboard())
        .await?;
    Ok(())
}

async fn help(bot: &Bot, msg: &Message) -> HandlerResult {
    let help_message = Command::descriptions().to_string();
    bot.send_message(msg.chat.id, help_message).await?;
    Ok(())
}

async fn admin(bot: &Bot, msg: &Message, context: &Context, token: &str) -> HandlerResult {
    if let Some(t) = &context.token {
        if t.lock().await.validate(token) && msg.chat.is_private() {
            context.database.set_admin(msg.chat.id.0).await?;
            bot.send_message(msg.chat.id, "Ok").await?;
        }
    }

    Ok(())
}

async fn delete_all_filters(
    bot: &Bot,
    msg: &Message,
    context: &Context,
    dialogue: &FilterDialogue,
    channel: Option<ChatId>,
) -> HandlerResult {
    let chat_id = user_or_channel_checked!(bot, msg.chat.id, channel, dialogue);
    let removed = context.database.remove_subscription(chat_id).await?;
    let message = if removed {
        "Deine Filter wurden entfernt."
    } else {
        "Es waren keine Filter aktiv."
    };
    bot.send_message(msg.chat.id, message).await?;
    Ok(())
}

async fn receive_channel_selection(
    bot: &Bot,
    dialogue: &FilterDialogue,
    msg: &Message,
    request_id: RequestId,
) -> HandlerResult {
    let Some(shared) = msg.shared_chat() else {
        return Ok(());
    };

    if shared.request_id != request_id {
        return Ok(());
    }

    // let text = if let Some(title) = &shared.title {
    //     format!("Channel „{title}” wurde ausgewählt!")
    // } else {
    let text = "Channel wurde ausgewählt!".to_string();
    // };

    dialogue
        .update(StateWithChannel {
            channel: Some(shared.chat_id),
            state: State::Start,
        })
        .await?;
    bot.send_message(msg.chat.id, text).await?;

    Ok(())
}

async fn receive_tag(
    bot: Bot,
    context: Arc<Context>,
    dialogue: FilterDialogue,
    previous_conditions: Vec<Condition>,
    msg: Message,
    channel: Option<ChatId>,
) -> HandlerResult {
    if let Some(tag_text) = msg.text() {
        let tag = match tag_text {
            "Speichern" => {
                let chat_id = user_or_channel_checked!(bot, msg.chat.id, channel, dialogue);

                context
                    .database
                    .update_filter(chat_id, &|filters| {
                        filters.push(Filter {
                            conditions: previous_conditions.clone(),
                        })
                    })
                    .await?;
                bot.send_message(msg.chat.id, "Filter gespeichert!").await?;
                dialogue.exit().await?;
                return Ok(());
            }
            "Drucksachen-Nummer" => Tag::Dsnr,
            "Art der Vorlage" => Tag::Art,
            "Antrag- oder Fragesteller:in" => Tag::Verfasser,
            "Federführendes Amt" => Tag::Federführend,
            "Beteiligtes Amt" => Tag::Beteiligt,
            "Gremium" => Tag::Gremium,
            _ => {
                bot.send_message(msg.chat.id, "Ungültiger Tag! Versuche es noch mal")
                    .reply_markup(tag_keyboard())
                    .await?;
                return Ok(());
            }
        };

        bot.send_message(msg.chat.id, "Wann soll die Bedingung erfüllt sein?")
            .reply_markup(negation_keyboard())
            .await?;
        dialogue
            .update(StateWithChannel {
                channel,
                state: State::ReceivingNegation {
                    previous_conditions,
                    tag,
                },
            })
            .await?;
    }
    Ok(())
}

async fn receive_negation(
    bot: Bot,
    dialogue: FilterDialogue,
    previous_conditions: Vec<Condition>,
    tag: Tag,
    msg: Message,
    channel: Option<ChatId>,
) -> HandlerResult {
    let negation = matches!(msg.text(), Some("Wenn das Pattern nicht zutrifft"));
    dialogue
        .update(StateWithChannel {
            state: State::ReceivingPattern {
                previous_conditions,
                tag,
                negation,
            },
            channel,
        })
        .await?;
    bot.send_message(msg.chat.id, "Gib nun ein Regex-Pattern ein.")
        .await?;
    Ok(())
}

async fn receive_pattern(
    bot: Bot,
    dialogue: FilterDialogue,
    mut previous_conditions: Vec<Condition>,
    tag: Tag,
    negate: bool,
    msg: Message,
    channel: Option<ChatId>,
) -> HandlerResult {
    if let Some(pattern) = msg.text() {
        match Regex::new(pattern) {
            Ok(regex) => {
                previous_conditions.push(Condition {
                    tag,
                    pattern: regex,
                    negate,
                });
                let text = format!(
                    "Bedingung hinzugefügt! Bisherige Bedingungen:\n{}\nGib einen weiteren Tag ein oder speichere den Filter.",
                    Filter {
                        conditions: previous_conditions.clone()
                    }
                );
                dialogue
                    .update(StateWithChannel {
                        state: State::ReceivingTag {
                            previous_conditions,
                        },
                        channel,
                    })
                    .await?;
                bot.send_message(msg.chat.id, text)
                    .reply_markup(tag_keyboard())
                    .await?;
            }
            Err(_) => {
                bot.send_message(
                    msg.chat.id,
                    "Ungültiges Regex-Muster. Bitte versuche es erneut:",
                )
                .await?;
            }
        }
    }
    Ok(())
}

fn create(
    bot: Bot,
    client: redis::Client,
    allris_url: AllrisUrl,
    admin_token: Option<AdminToken>,
) -> Dispatcher<Bot, HandlerError, teloxide::dispatching::DefaultKey> {
    let connection = DatabaseConnection::new(client, Some(Duration::from_secs(6))).shared();

    let context = Arc::new(Context {
        database: connection,
        allris_url,
        token: admin_token.map(Mutex::new),
    });

    let handler = Update::filter_message()
        .enter_dialogue::<Message, Context, StateWithChannel>()
        .endpoint(handle_message);

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![context])
        .default_handler(|update| async move {
            log::info!("Missed update: {:?}", update.kind);
        })
        .build()
}

pub struct DispatcherTask {
    token: Option<ShutdownToken>,
}

impl DispatcherTask {
    /// Creates a dispatcher to handle the bot's incoming messages.
    pub fn new(
        bot: Bot,
        db: redis::Client,
        allris_url: AllrisUrl,
        admin_token: Option<AdminToken>,
    ) -> Self {
        let mut dispatcher = create(bot, db, allris_url, admin_token);
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
