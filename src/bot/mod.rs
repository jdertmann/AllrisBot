#[macro_use]
mod macros;

mod command_cancel;
mod command_help;
mod command_new_rule;
mod command_remove_all_rules;
mod command_remove_rule;
mod command_rules;
mod command_start;
mod command_target;
mod get_updates;
mod keyboard;

use std::fmt::Display;
use std::sync::Arc;

use frankenstein::AsyncTelegramApi;
use frankenstein::methods::{
    GetChatAdministratorsParams, SetMyCommandsParams, SetMyDescriptionParams,
    SetMyShortDescriptionParams,
};
use frankenstein::types::{BotCommand, BotCommandScope, Message};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use self::command_new_rule::{PatternInput, TagSelection};
use self::command_remove_all_rules::ConfirmRemoveAllFilters;
use self::command_remove_rule::RemoveFilterSelection;
use self::command_target::ChannelSelection;
use self::get_updates::UpdateHandler;
use self::keyboard::remove_keyboard;
use crate::database::{self, SharedDatabaseConnection};
use crate::escape_html;

const SHORT_DESCRIPTION: &str = "Dieser Bot benachrichtigt dich, wenn im Ratsinformationssystem der Stadt Bonn neue Vorlagen veröffentlicht werden.";

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("User {0} is not admin of channel {1}")]
    NotChannelAdmin(i64, i64),
    #[error("Unexpected message")]
    UnexpectedMessage,
    #[error("Topics not yet supported")]
    TopicsNotSupported,
    #[error("Unknown command {0}")]
    UnknownCommand(String),
    #[error("Telegram error: {0}")]
    Telegram(#[from] frankenstein::Error),
    #[error("Database error: {0}")]
    Database(#[from] database::Error),
}

type HandlerResult<T = ()> = Result<T, Error>;

macro_rules! commands {
    ($($cmd:ident),*) => {
        async fn handle_command(cx: HandleMessage<'_>, cmd: &str, param: Option<&str>) -> HandlerResult {
            let cmd = cmd.to_ascii_lowercase();
            match cmd.as_str() {
                $(cmd if cmd == $cmd::COMMAND.name => $cmd::handle_command(cx, param).await,)+
                _ => Err(Error::UnknownCommand(cmd))
            }
        }

        fn commands() -> &'static [&'static Command] {
            &[
                $(&$cmd::COMMAND),+
            ]
        }
    };
    (@param param) => { , param };
}

macro_rules! states {
    ($enum:ident; $($state:ident), * $(,)?) => {
        #[derive(Clone, Default, Debug, Serialize, Deserialize, PartialEq, Eq)]
        enum $enum {
            #[default]
            Initial,
            $($state($state)),*
        }

        impl $enum {
            async fn handle_message(self, cx: HandleMessage<'_>, channel: Option<SelectedChannel>) -> HandlerResult {
                match self {
                    Self::Initial => Err(Error::UnexpectedMessage),
                    $(Self::$state(state) => state.handle_message(cx, channel).await),*
                }
            }
        }

        $(
        impl From<$state> for $enum {
            fn from(x: $state) -> Self {
                Self::$state(x)
            }
        }
        )*
    };
}

struct Command {
    name: &'static str,
    description: &'static str,
    group_admin: bool,
    group_member: bool,
    private_chat: bool,

    #[allow(unused)]
    admin: bool,
}

commands! {
    command_new_rule,
    command_rules,
    command_remove_rule,
    command_remove_all_rules,

    command_target,

    command_cancel,
    command_help,
    command_start
}

states! {
    DialogueState;
    ConfirmRemoveAllFilters,
    PatternInput,
    TagSelection,
    ChannelSelection,
    RemoveFilterSelection
}

#[derive(Debug)]
struct MessageHandler {
    bot: crate::Bot,
    database: SharedDatabaseConnection,
    command_regex: Regex,
    owner: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SelectedChannel {
    chat_id: i64,
    username: Option<String>,
    title: Option<String>,
}

impl SelectedChannel {
    fn channel_id(&self) -> u64 {
        (self.chat_id + 1_000_000_000_000).unsigned_abs()
    }

    fn hyperlink_html(&self) -> impl Display {
        struct Link<'a>(&'a SelectedChannel);

        impl Display for Link<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                // make sure that username only contains allowed chars,
                // then we know it's already html escaped
                let username = self
                    .0
                    .username
                    .as_deref()
                    .filter(|name| name.chars().all(|x| x.is_ascii_alphanumeric() || x == '_'));

                write!(f, "<a href=\"")?;

                if let Some(username) = username {
                    write!(f, "https://t.me/{username}")?;
                } else {
                    write!(f, "https://t.me/c/{}", self.0.channel_id())?;
                };

                write!(f, "\">„")?;

                if let Some(title) = &self.0.title {
                    let title = escape_html(title);
                    write!(f, "{title}")?;
                } else if let Some(username) = username {
                    write!(f, "@{username}")?;
                } else {
                    write!(f, "<unbekannt>")?;
                };

                write!(f, "“</a>")
            }
        }

        Link(self)
    }

    fn chat_selection_html(channel: &Option<Self>) -> impl Display {
        struct ChatSelection<'a>(&'a Option<SelectedChannel>);

        impl Display for ChatSelection<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self.0 {
                    Some(channel) => write!(f, "📢 {}", channel.hyperlink_html()),
                    None => write!(f, "💬 Dieser Chat"),
                }
            }
        }

        ChatSelection(channel)
    }

    fn chat_selection_html_accusative(channel: &Option<Self>) -> impl Display {
        struct ChatSelection<'a>(&'a Option<SelectedChannel>);

        impl Display for ChatSelection<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self.0 {
                    Some(channel) => write!(f, "den Kanal {}", channel.hyperlink_html()),
                    None => write!(f, "diesen Chat"),
                }
            }
        }

        ChatSelection(channel)
    }
}

#[derive(Clone, Default, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Dialogue {
    channel: Option<SelectedChannel>,
    state: DialogueState,
}

impl MessageHandler {
    async fn set_my_commands(
        &self,
        scope: BotCommandScope,
        filter: impl Fn(&Command) -> bool,
    ) -> HandlerResult {
        let commands = commands()
            .iter()
            .copied()
            .filter(|cmd| filter(cmd))
            .map(|cmd| {
                BotCommand::builder()
                    .command(cmd.name)
                    .description(cmd.description)
                    .build()
            })
            .collect();

        let params = SetMyCommandsParams::builder()
            .scope(scope)
            .commands(commands)
            .build();

        self.bot.set_my_commands(&params).await?;

        Ok(())
    }

    pub async fn prepare_bot(&self) -> HandlerResult {
        self.set_my_commands(BotCommandScope::AllPrivateChats, |cmd| cmd.private_chat)
            .await?;
        self.set_my_commands(BotCommandScope::AllGroupChats, |cmd| cmd.group_member)
            .await?;
        self.set_my_commands(BotCommandScope::AllChatAdministrators, |cmd| {
            cmd.group_admin
        })
        .await?;

        let params = SetMyDescriptionParams::builder()
            .description(SHORT_DESCRIPTION)
            .build();
        self.bot.set_my_description(&params).await?;

        let params = SetMyShortDescriptionParams::builder()
            .short_description(SHORT_DESCRIPTION)
            .build();
        self.bot.set_my_short_description(&params).await?;

        Ok(())
    }

    async fn new(
        bot: crate::Bot,
        database: SharedDatabaseConnection,
        owner: Option<String>,
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

        let handler = Self {
            bot,
            database,
            command_regex,
            owner,
        };

        handler.prepare_bot().await?;

        Ok(handler)
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

impl HandleMessage<'_> {
    async fn handle(self) {
        let result = async {
            if let Some(new_chat_id) = self.message.migrate_to_chat_id {
                return self.handle_migrate_to_chat_id(new_chat_id).await;
            }

            if let Some(text) = &self.message.text {
                if self.message.message_thread_id.is_some() {
                    return Err(Error::TopicsNotSupported);
                }

                if let Some((cmd, param)) = self.inner.parse_command(text) {
                    return handle_command(self, cmd, param).await;
                }
            }

            let dialogue = self.get_dialogue().await?;

            dialogue.state.handle_message(self, dialogue.channel).await
        }
        .await;

        if let Err(e) = result {
            let _ = self.handle_error(e).await;
        }
    }

    async fn handle_error(self, e: Error) {
        let warn = match &e {
            Error::NotChannelAdmin(_, _) => {
                _ = self.remove_dialogue().await;
                _ = respond!(
                    self,
                    text = "Du hast für diesen Channel nicht die notwendigen Rechte!",
                    reply_markup = remove_keyboard()
                )
                .await;
                true
            }
            Error::TopicsNotSupported => {
                _ = respond!(self, text = "Topics werden noch nicht unterstützt").await;
                false
            }
            Error::UnexpectedMessage => false,
            Error::UnknownCommand(_) => {
                _ = respond!(self, text = "Unbekannter Befehl!").await;
                false
            }
            Error::Telegram(_) => {
                // not responding, as it will presumably not work
                true
            }
            _ => {
                _ = respond!(self, text = "Ein interner Fehler ist aufgetreten 😢").await;
                true
            }
        };

        if warn {
            log::warn!("{e}");
        } else {
            log::info!("{e}");
        }
    }

    async fn update_dialogue(
        self,
        state: impl Into<DialogueState>,
        channel: Option<SelectedChannel>,
    ) -> HandlerResult {
        let dialogue = Dialogue {
            state: state.into(),
            channel,
        };

        self.inner
            .database
            .update_dialogue(self.chat_id(), &dialogue)
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

    async fn reset_dialogue(self, channel: Option<SelectedChannel>) -> HandlerResult {
        self.update_dialogue(DialogueState::default(), channel)
            .await
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

    async fn selected_chat(self, channel: &Option<SelectedChannel>) -> HandlerResult<i64> {
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
                .chat_id(channel.chat_id)
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
                Ok(channel.chat_id)
            } else {
                Err(Error::NotChannelAdmin(self.chat_id(), channel.chat_id))
            }
        } else {
            Ok(self.chat_id())
        }
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
    owner: Option<String>,
    shutdown: oneshot::Receiver<()>,
) {
    let message_handler = MessageHandler::new(bot.clone(), database, owner)
        .await
        .unwrap();

    get_updates::handle_updates(bot, Arc::new(message_handler), shutdown).await
}
