use frankenstein::AsyncTelegramApi;
use frankenstein::methods::GetChatAdministratorsParams;
use serde::{Deserialize, Serialize};
use telegram_message_builder::{WriteToMessage, concat, text_link};

use crate::ChatId;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelectedChannel {
    pub chat_id: ChatId,
    pub username: Option<String>,
    pub title: Option<String>,
}

impl SelectedChannel {
    /// Converts internal Telegram chat_id to the public channel ID format used in links.
    pub fn channel_id(&self) -> u64 {
        (self.chat_id + 1_000_000_000_000).unsigned_abs()
    }

    /// Returns a formatted Telegram hyperlink for the channel.
    pub fn hyperlink(&self) -> impl WriteToMessage + '_ {
        let link = if let Some(username) = &self.username {
            format!("https://t.me/{}", username)
        } else {
            format!("https://t.me/c/{}", self.channel_id())
        };

        let title = telegram_message_builder::from_fn(|msg| {
            if let Some(title) = &self.title {
                msg.write(title)
            } else if let Some(username) = &self.username {
                msg.write(concat!("@", username))
            } else {
                msg.write("<unbekannt>")
            }
        });

        text_link(link, concat!("„", title, "“"))
    }

    /// Writes the current chat selection (with an icon).
    pub fn chat_selection(channel: &Option<Self>) -> impl WriteToMessage {
        telegram_message_builder::from_fn(move |msg| match channel {
            Some(channel) => {
                msg.write("📢 ")?;
                msg.write(channel.hyperlink())
            }
            None => msg.write("💬 Dieser Chat"),
        })
    }

    /// Writes the current chat selection in accusative form (e.g., "den Kanal").
    pub fn chat_selection_accusative(channel: &Option<Self>) -> impl WriteToMessage {
        telegram_message_builder::from_fn(move |msg| match channel {
            Some(channel) => {
                msg.write("den Kanal ")?;
                msg.write(channel.hyperlink())
            }
            None => msg.write("diesen Chat"),
        })
    }
}

pub async fn selected_chat<B: AsyncTelegramApi>(
    bot: &B,
    chat_id: ChatId,
    channel: &Option<SelectedChannel>,
) -> Result<Option<ChatId>, B::Error> {
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

        let authorized = bot
            .get_chat_administrators(&params)
            .await?
            .result
            .iter()
            .filter_map(|member| user!(member, Administrator, Creator))
            .any(|user| user.id.try_into() == Ok(chat_id));

        if authorized {
            Ok(Some(channel.chat_id))
        } else {
            Ok(None)
        }
    } else {
        Ok(Some(chat_id))
    }
}
