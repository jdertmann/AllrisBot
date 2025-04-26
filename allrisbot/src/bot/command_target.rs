use frankenstein::types::{ChatAdministratorRights, KeyboardButtonRequestChat};
use serde::{Deserialize, Serialize};

use super::keyboard::{Button, Choice, Choices, remove_keyboard};
use super::{Command, Error, HandleMessage, HandlerResult, SelectedChannel};

pub const COMMAND: Command = Command {
    name: "ziel",
    description: "Lege fest, für welchen Chat du Benachrichtigungen konfigurieren möchtest",

    group_admin: false,
    group_member: false,
    private_chat: true,
    admin: true,
};

#[derive(Debug)]
enum Action {
    PrivateChat,
    ChannelShared(SelectedChannel),
}

#[derive(PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
enum Alternatives {
    PrivateChat,
    RequestChannel(i32),
}

impl<'a> Choice<'a> for &'a Alternatives {
    type Action = Action;
    fn button(&self) -> Button<'a, Self> {
        match self {
            Alternatives::PrivateChat => Button::Text {
                text: "Dieser Chat".into(),
                action: |_| Action::PrivateChat,
            },
            &&Alternatives::RequestChannel(request_id) => Button::RequestChat {
                text: "Channel auswählen".into(),
                request_id,
                request_chat: |request_id| {
                    let permissions = ChatAdministratorRights::builder()
                        .is_anonymous(false)
                        .can_manage_chat(false)
                        .can_delete_messages(false)
                        .can_restrict_members(false)
                        .can_promote_members(false)
                        .can_change_info(false)
                        .can_invite_users(false)
                        .can_manage_video_chats(false)
                        .can_post_messages(true)
                        .build();

                    KeyboardButtonRequestChat::builder()
                        .request_id(request_id)
                        .chat_is_channel(true)
                        .user_administrator_rights(permissions)
                        .bot_administrator_rights(permissions)
                        .request_title(true)
                        .request_username(true)
                        .build()
                },
                action: |x| Action::ChannelShared(x),
            },
        }
    }
}

#[derive(PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct ChannelSelection {
    buttons: Vec<Alternatives>,
}

impl ChannelSelection {
    fn new(request_id: i32, with_reset: bool) -> Self {
        let mut buttons = if with_reset {
            vec![Alternatives::PrivateChat]
        } else {
            vec![]
        };

        buttons.push(Alternatives::RequestChannel(request_id));

        ChannelSelection { buttons }
    }

    pub(super) async fn handle_message(
        self,
        cx: HandleMessage<'_>,
        _: Option<SelectedChannel>,
    ) -> HandlerResult {
        match self.buttons.match_action(cx.message) {
            Some(Action::ChannelShared(channel)) => self.handle_chat_shared(cx, channel).await,
            Some(Action::PrivateChat) => self.handle_reset(cx).await,
            None if cx.message.text.is_some() => self.handle_unexpected_text(cx).await,
            _ => Err(Error::UnexpectedMessage),
        }
    }

    async fn handle_unexpected_text(&self, cx: HandleMessage<'_>) -> HandlerResult {
        let text = format!(
            "️Bitte verwende die Schaltflächen, um einen Chat auszuwählen, oder sende /{} zum Abbrechen",
            super::command_cancel::COMMAND.name
        );

        respond!(
            cx,
            text = text,
            reply_markup = self.buttons.keyboard_markup()
        )
        .await
    }

    async fn handle_chat_shared(
        &self,
        cx: HandleMessage<'_>,
        channel: SelectedChannel,
    ) -> HandlerResult {
        let text = format!(
            "✅ Der Kanal {} wurde ausgewählt!\n\n\
             Du kannst nun die Einstellungen für diesen Channel ändern. \
             Führe /{} erneut aus, um die Auswahl zu ändern oder zurückzusetzen.",
            channel.hyperlink_html(),
            COMMAND.name
        );

        cx.reset_dialogue(Some(channel)).await?;
        respond_html!(cx, text, reply_markup = remove_keyboard()).await
    }

    async fn handle_reset(&self, cx: HandleMessage<'_>) -> HandlerResult {
        let text = "✅ Du kannst nun wieder Einstellungen für diesen privaten Chat vornehmen.";

        cx.reset_dialogue(None).await?;
        respond!(cx, text, reply_markup = remove_keyboard()).await
    }
}

pub async fn handle_command(cx: HandleMessage<'_>, _: Option<&str>) -> HandlerResult {
    if cx.message.chat.id < 0 {
        respond!(
            cx,
            text = "Dieser Befehl wird nur in privaten Chats unterstützt!"
        )
        .await?;
        return Ok(());
    }

    let request_id = cx.message.message_id;

    let dialogue = cx.get_dialogue().await?;
    let current_channel = &dialogue.channel;
    let state = ChannelSelection::new(request_id, current_channel.is_some());
    let reply_markup = state.buttons.keyboard_markup();

    let text = if let Some(channel) = current_channel {
        &format!(
            "Aktuelle Auswahl: 📢 {}\n\n\
             Du kannst zu diesem privaten Chat zurückwechseln oder einen anderen Kanal wählen:",
            channel.hyperlink_html()
        )
    } else {
        "Aktuelle Auswahl: 💬 Dieser Chat\n\n\
         Du kannst stattdessen auch einen Kanal auswählen:"
    };

    cx.update_dialogue(state, dialogue.channel).await?;
    respond_html!(cx, text, reply_markup).await
}
