mod admin;
#[macro_use]
mod commands;
mod chat_shared;
mod get_updates;
mod text_received;

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use frankenstein::AsyncTelegramApi;
use frankenstein::methods::GetChatAdministratorsParams;
use frankenstein::types::{KeyboardButton, Message};
use get_updates::UpdateHandler;
use regex::Regex;
use serde::{Deserialize, Serialize};

use self::admin::AdminToken;
use crate::allris::AllrisUrl;
use crate::database::{self, SharedDatabaseConnection};
use crate::types::{Condition, Tag};

#[derive(Debug)]
struct MessageHandler {
    allris_url: AllrisUrl,
    bot: crate::Bot,
    database: SharedDatabaseConnection,
    admin_token: Option<Mutex<AdminToken>>,
    command_regex: Regex,
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("User {0} is not permitted to run command {1}")]
    Unauthorized(i64, String),
    #[error("User {0} is not admin of channel {1}")]
    NotChannelAdmin(i64, i64),
    #[error("Unexpected message")]
    UnexpectedMessage,
    #[error("Topics not yet supported")]
    TopicsNotSupported,
    #[error("Invalid input")]
    InvalidInput,
    #[error("Unknown command {0}")]
    UnknownCommand(String),
    #[error("Telegram error: {0}")]
    Telegram(#[from] frankenstein::Error),
    #[error("Database error: {0}")]
    Database(#[from] database::Error),
}

type HandlerResult<T = ()> = Result<T, Error>;

#[derive(Clone, Default, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum DialogueState {
    #[default]
    Initial,
    ReceiveChannelSelection {
        request_id: i32,
    },
    ReceiveTag {
        previous_conditions: Vec<Condition>,
    },
    ReceiveNegation {
        previous_conditions: Vec<Condition>,
        tag: Tag,
    },
    ReceivePattern {
        previous_conditions: Vec<Condition>,
        tag: Tag,
        negation: bool,
    },
    DeleteFilter,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Dialogue {
    channel: Option<i64>,
    state: DialogueState,
}

impl MessageHandler {
    async fn new(
        bot: crate::Bot,
        database: SharedDatabaseConnection,
        allris_url: AllrisUrl,
        admin_token: Option<AdminToken>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let pattern = if let Some(username) = bot.get_me().await?.result.username {
            &format!(
                "^/([a-z0-9_]+)(?:@{})?(?:\\s+(.*))?$",
                regex::escape(&username)
            )
        } else {
            log::warn!("Bot has no username!");
            "^/([a-z0-9_]+)(?:\\s+(.*))?$"
        };

        let command_regex = regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .dot_matches_new_line(true)
            .build()?;

        Ok(Self {
            allris_url,
            bot,
            admin_token: admin_token.map(Mutex::new),
            database,
            command_regex,
        })
    }

    fn parse_command<'a>(&self, text: &'a str) -> Option<(&'a str, Option<&'a str>)> {
        self.command_regex.captures(text).map(|captures| {
            let command = captures.get(1).unwrap().as_str();
            let params = captures.get(2).map(|m| m.as_str());
            (command, params)
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct HandleMessage<'a> {
    message: &'a Message,
    inner: &'a MessageHandler,
}

trait Selection<T> {
    fn options(&self) -> impl Iterator<Item = (Cow<'static, str>, T)>;

    fn matches(&self, response: &str) -> Option<T> {
        self.options().find(|x| x.0 == response).map(|x| x.1)
    }

    fn buttons(&self) -> impl Iterator<Item = KeyboardButton> {
        self.options()
            .map(|(text, _)| KeyboardButton::builder().text(text).build())
    }
}

impl HandleMessage<'_> {
    async fn handle(self) {
        let result = if let Some(new_chat_id) = self.message.migrate_to_chat_id {
            self.handle_migrate_to_chat_id(new_chat_id).await
        } else if let Some(text) = &self.message.text {
            if let Some((cmd, param)) = self.inner.parse_command(text) {
                self.handle_command(&cmd, param).await
            } else {
                self.handle_text(text).await
            }
        } else if let Some(chat) = &self.message.chat_shared {
            self.handle_chat_shared(chat).await
        } else {
            Err(Error::UnexpectedMessage)
        };

        if let Err(e) = result {
            let _ = self.handle_error(e).await;
        }
    }

    async fn handle_error(self, e: Error) {
        let warn = match &e {
            Error::NotChannelAdmin(_, _) => {
                let _ = self.remove_dialogue().await;
                let _ = self
                    .respond("Du hast für diesen Channel nicht die notwendigen Rechte!")
                    .await;
                true
            }
            Error::TopicsNotSupported => {
                let _ = self.respond("Topics werden noch nicht unterstützt").await;
                false
            }
            Error::InvalidInput => {
                let _ = self.respond("Ungültige Eingabe").await;
                false
            }
            Error::UnexpectedMessage => false,
            Error::Unauthorized(_, _) | Error::UnknownCommand(_) => {
                let _ = self.respond("Unbekannter Befehl!").await;
                matches!(e, Error::Unauthorized(_, _))
            }
            Error::Telegram(_) => {
                // not responding, as it will presumably not work
                true
            }
            _ => {
                let _ = self.respond("Ein interner Fehler ist aufgetreten :(").await;
                true
            }
        };

        if warn {
            log::warn!("{e}");
        } else {
            log::info!("{e}");
        }
    }

    async fn with_dialogue(
        self,
        f: impl AsyncFnOnce(Dialogue) -> HandlerResult<Dialogue>,
    ) -> HandlerResult {
        let dialogue = self.get_dialogue().await?;
        let updated = f(dialogue.clone()).await?;
        if updated != dialogue {
            self.update_dialogue(&updated).await?;
        }
        Ok(())
    }

    async fn update_dialogue(self, dialogue: &Dialogue) -> HandlerResult {
        self.inner
            .database
            .update_dialogue(self.chat_id(), dialogue)
            .await?;
        Ok(())
    }

    async fn get_dialogue(self) -> HandlerResult<Dialogue> {
        let dialogue = self
            .inner
            .database
            .get_dialogue(self.chat_id())
            .await?
            .unwrap_or_default();
        Ok(dialogue)
    }

    async fn remove_dialogue(self) -> HandlerResult<()> {
        self.inner.database.remove_dialogue(self.chat_id()).await?;
        Ok(())
    }

    async fn handle_migrate_to_chat_id(self, new_chat_id: i64) -> HandlerResult {
        log::info!("Migrating chat {} to {new_chat_id}", self.chat_id());
        self.inner
            .database
            .migrate_chat(self.chat_id(), new_chat_id)
            .await?;
        Ok(())
    }

    fn chat_id(self) -> i64 {
        self.message.chat.id
    }

    async fn selected_chat_id(self, channel: Option<i64>) -> HandlerResult<i64> {
        macro_rules! user {
            ($member:expr, $($variant:ident),+) => {
                match $member {
                    $(frankenstein::types::ChatMember::$variant(x) => {
                        Some(&x.user)
                    })+,
                    _ => None
                }
            };
        }

        if let Some(channel) = channel {
            let params = GetChatAdministratorsParams::builder()
                .chat_id(channel)
                .build();

            let authorized = self
                .inner
                .bot
                .get_chat_administrators(&params)
                .await?
                .result
                .iter()
                .filter_map(|member| user!(member, Administrator, Creator))
                .any(|user| user.id.try_into() == Ok(self.chat_id()));

            if authorized {
                Ok(channel)
            } else {
                Err(Error::NotChannelAdmin(self.chat_id(), channel))
            }
        } else {
            Ok(self.chat_id())
        }
    }

    async fn require_admin_permissions(self, cmd: &str) -> HandlerResult<()> {
        let authorized = self.inner.database.is_admin(self.chat_id()).await?;

        if authorized {
            Ok(())
        } else {
            Err(Error::Unauthorized(self.chat_id(), cmd.into()))
        }
    }

    async fn respond(self, text: &str) -> HandlerResult {
        let params = response_params!(self).text(text).build();
        self.inner.bot.send_message(&params).await?;
        Ok(())
    }
}

impl UpdateHandler for Arc<MessageHandler> {
    async fn handle_message(self, message: Message) {
        HandleMessage {
            message: &message,
            inner: &self,
        }
        .handle()
        .await
    }
}

pub async fn run(
    bot: crate::Bot,
    database: SharedDatabaseConnection,
    allris_url: AllrisUrl,
    generate_admin_token: bool,
    shutdown: oneshot::Receiver<()>,
) {
    let admin_token = generate_admin_token.then(|| {
        let token = AdminToken::new();
        println!("Admin token (valid for 10 minutes): {token}");
        token
    });

    let message_handler = MessageHandler::new(bot.clone(), database, allris_url, admin_token)
        .await
        .unwrap();

    get_updates::handle_updates(bot, Arc::new(message_handler), shutdown).await
}
