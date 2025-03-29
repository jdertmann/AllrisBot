use std::sync::Arc;
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use teloxide::dispatching::ShutdownToken;
use teloxide::dispatching::dialogue::InMemStorage;
use teloxide::prelude::*;
use teloxide::types::{KeyboardButton, KeyboardMarkup, ReplyMarkup};
use teloxide::utils::command::BotCommands;
use tokio::sync::Mutex;

use crate::Bot;
use crate::admin::AdminToken;
use crate::database::{DatabaseConnection, SharedDatabaseConnection};
use crate::types::{Condition, Filter, Tag};

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
    #[command(description = "Eine neue Filterregel hinzufügen.")]
    DeleteFilter,
    #[command(description = "Eine neue Filterregel hinzufügen.")]
    Stop,
    #[command(description = "zeige diesen Text.")]
    Help,
    #[command(hide)]
    Admin(String),
}

struct Context {
    connection: SharedDatabaseConnection,
    token: Option<Mutex<AdminToken>>,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub enum State {
    #[default]
    Start,
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
}

type FilterDialogue = Dialogue<State, InMemStorage<State>>;
type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

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
        vec![KeyboardButton {
            text: "Wenn das Pattern zutrifft".into(),
            request: None,
        }],
        vec![KeyboardButton {
            text: "Wenn das Pattern nicht zutrifft".into(),
            request: None,
        }],
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

async fn handle_command(
    bot: Bot,
    dialogue: FilterDialogue,
    msg: Message,
    command: Command,
    context: Arc<Context>,
) -> HandlerResult {
    match command {
        Command::AddFilter => {
            dialogue
                .update(State::ReceivingTag {
                    previous_conditions: vec![],
                })
                .await?;
            let text = "Lass uns einen Filter einrichten! Du kannst den Filter auch sofort speichern, um alle Vorlagen einzuschließen.\n\nBitte gib den ersten Tag ein:";
            bot.send_message(msg.chat.id, text)
                .reply_markup(tag_keyboard())
                .await?;
        }
        Command::ListFilters => todo!(),
        Command::DeleteFilter => todo!(),
        Command::Stop => todo!(),
        Command::Help => {
            let commands = Command::descriptions().to_string();
            bot.send_message(msg.chat.id, commands).await?;
        }
        Command::Admin(token) => {
            if let Some(t) = &context.token {
                if t.lock().await.validate(&token) {
                    context.connection.set_admin(msg.chat.id.0).await?;
                    bot.send_message(msg.chat.id, "Ok").await?;
                }
            }
        }
    }

    Ok(())
}

async fn receive_tag(
    bot: Bot,
    context: Arc<Context>,
    dialogue: FilterDialogue,
    (previous_conditions,): (Vec<Condition>,),
    msg: Message,
) -> HandlerResult {
    if let Some(tag_text) = msg.text() {
        let tag = match tag_text {
            "Speichern" => {
                context
                    .connection
                    .update_filter(msg.chat.id.0, &|filters| {
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
            .update(State::ReceivingNegation {
                previous_conditions,
                tag,
            })
            .await?;
    }
    Ok(())
}

async fn receive_negation(
    bot: Bot,
    dialogue: FilterDialogue,
    (previous_conditions, tag): (Vec<Condition>, Tag),
    msg: Message,
) -> HandlerResult {
    let negation = matches!(msg.text(), Some("Wenn das Pattern nicht zutrifft"));
    dialogue
        .update(State::ReceivingPattern {
            previous_conditions,
            tag,
            negation,
        })
        .await?;
    bot.send_message(msg.chat.id, "Gib nun ein Regex-Pattern ein.")
        .await?;
    Ok(())
}

async fn receive_pattern(
    bot: Bot,
    dialogue: FilterDialogue,
    (mut previous_conditions, tag, negate): (Vec<Condition>, Tag, bool),
    msg: Message,
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
                    .update(State::ReceivingTag {
                        previous_conditions,
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
    admin_token: Option<AdminToken>,
) -> Dispatcher<Bot, Box<dyn std::error::Error + Send + Sync>, teloxide::dispatching::DefaultKey> {
    let connection = SharedDatabaseConnection::new(DatabaseConnection::new(
        client,
        Some(Duration::from_secs(6)),
    ));

    let context = Arc::new(Context {
        connection,
        token: admin_token.map(Mutex::new),
    });

    let handler = Update::filter_message()
        .enter_dialogue::<Message, InMemStorage<State>, State>()
        .branch(
            dptree::case![State::Start]
                .filter_command::<Command>()
                .endpoint(handle_command),
        )
        .branch(
            dptree::case![State::ReceivingNegation {
                previous_conditions,
                tag
            }]
            .endpoint(receive_negation),
        )
        .branch(
            dptree::case![State::ReceivingPattern {
                previous_conditions,
                tag,
                negation
            }]
            .endpoint(receive_pattern),
        )
        .branch(
            dptree::case![State::ReceivingTag {
                previous_conditions,
            }]
            .endpoint(receive_tag),
        );

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![context, InMemStorage::<State>::new()])
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
    pub fn new(bot: Bot, db: redis::Client, admin_token: Option<AdminToken>) -> Self {
        let mut dispatcher = create(bot, db, admin_token);
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
