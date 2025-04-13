use frankenstein::types::{ChatShared, ReplyKeyboardRemove, ReplyMarkup};
use frankenstein::{AsyncTelegramApi as _, ParseMode};

use super::{Dialogue, DialogueState, Error, HandleMessage, HandlerResult};
use crate::escape_html;

impl HandleMessage<'_> {
    pub(super) async fn handle_chat_shared(self, chat: &ChatShared) -> HandlerResult {
        self.with_dialogue(async move |dialogue| match dialogue.state {
            DialogueState::ReceiveChannelSelection { request_id }
                if request_id == chat.request_id =>
            {
                Ok(Dialogue {
                    channel: Some(chat.chat_id),
                    state: DialogueState::Initial,
                })
            }
            _ => Err(Error::UnexpectedMessage),
        })
        .await?;

        let sentence = " wurde ausgewählt!\n\nDu kannst nun die Einstellungen für diesen Channel ändern. \
                            Führe /selectchannel erneut aus, um die Auswahl zu ändern oder \
                            zurückzusetzen.";
        let channel_id = -chat.chat_id - 1_000_000_000_000;

        let text = if let Some(title) = &chat.title {
            let title = escape_html(title);
            format!("Der Channel [*{title}*](https://t.me/c/{channel_id}){sentence}",)
        } else {
            format!("[Der Channel](https://t.me/c/{channel_id}){sentence}",)
        };

        let reply_markup = ReplyKeyboardRemove::builder().remove_keyboard(true).build();
        let params = response_params!(self)
            .text(&text)
            .parse_mode(ParseMode::Html)
            .reply_markup(ReplyMarkup::ReplyKeyboardRemove(reply_markup))
            .build();

        self.inner.bot.send_message(&params).await?;

        Ok(())
    }
}
