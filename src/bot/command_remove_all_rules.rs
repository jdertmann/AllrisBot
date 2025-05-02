use serde::{Deserialize, Serialize};
use telegram_message_builder::{WriteToMessage, concat};

use super::keyboard::{Button, Choice, Choices};
use super::{Command, HandleMessage, HandlerResult, SelectedChannel};
use crate::bot::keyboard::remove_keyboard;

pub const COMMAND: Command = Command {
    name: "alle_regeln_loeschen",
    description: "Entferne alle Regeln",

    group_admin: true,
    group_member: true,
    private_chat: true,
    admin: true,
};

#[derive(Clone, Default, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfirmRemoveAllFilters(());

#[derive(Debug, Copy, Clone)]
struct ConfirmChoice(bool);

impl<'a> Choice<'a> for ConfirmChoice {
    type Action = bool;

    fn button(&self) -> Button<'a, Self> {
        let text = if self.0 {
            "⚠️ Ja, alles löschen!"
        } else {
            "Abbrechen"
        };

        Button::Text {
            text: text.into(),
            action: |x| x.0,
        }
    }
}

fn buttons() -> &'static [ConfirmChoice; 2] {
    &[ConfirmChoice(true), ConfirmChoice(false)]
}

impl ConfirmRemoveAllFilters {
    pub(super) async fn handle_message(
        self,
        cx: HandleMessage<'_>,
        channel: Option<SelectedChannel>,
    ) -> HandlerResult {
        let chat_id = cx.selected_chat(&channel).await?;

        match buttons().match_action(cx.message) {
            Some(true) => {
                let removed = cx.inner.database.remove_subscription(chat_id).await?;

                let text = if removed {
                    "✅ Deine Regeln wurden gelöscht!"
                } else {
                    "❌ Die Regeln konnten leider nicht gelöscht werden. Bitte versuche es erneut."
                };

                if channel.is_none() {
                    cx.remove_dialogue().await?;
                } else {
                    cx.reset_dialogue(channel).await?;
                }

                respond!(cx, text, reply_markup = remove_keyboard()).await
            }
            _ => {
                cx.reset_dialogue(channel).await?;

                respond!(
                    cx,
                    text = "Der Vorgang wurde abgebrochen!",
                    reply_markup = remove_keyboard()
                )
                .await
            }
        }
    }
}

pub async fn handle_command(cx: HandleMessage<'_>, _: Option<&str>) -> HandlerResult {
    let dialogue = cx.get_dialogue().await?;
    let chat_id = cx.selected_chat(&dialogue.channel).await?;
    let filters = cx.inner.database.get_filters(chat_id).await?;

    let (text, entities) = {
        let target = SelectedChannel::chat_selection_accusative(&dialogue.channel);

        if filters.is_empty() {
            let (text, entities) =
                concat!("Zur Zeit sind keine Regeln für ", target, " aktiv!").to_message()?;
            return respond!(cx, text, entities, reply_markup = remove_keyboard()).await;
        }

        concat!(
            "🗑️ Du bist dabei, alle Regeln für ",
            target,
            " zu entfernen.\n\n",
            "Bist du sicher? Danach bekommst du erst mal keine Benachrichtigungen mehr."
        )
        .to_message()?
    };

    let state = ConfirmRemoveAllFilters(());
    cx.update_dialogue(state, dialogue.channel).await?;
    respond!(
        cx,
        text,
        entities,
        reply_markup = buttons().keyboard_markup()
    )
    .await
}
