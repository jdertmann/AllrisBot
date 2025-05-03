use bot_utils::Command;
use bot_utils::channel::SelectedChannel;
use bot_utils::keyboard::{Button, Choice, Choices, remove_keyboard};
use serde::{Deserialize, Serialize};
use telegram_message_builder::{MessageBuilder, WriteToMessage, bold, concat};

use super::{HandleMessage, HandlerResult};
use crate::types::Filter;

pub const COMMAND: Command = Command {
    name: "regel_loeschen",
    description: "Lösche eine bestehende Regel",

    group_admin: true,
    group_member: true,
    private_chat: true,
    admin: true,
};

#[derive(Clone, Default, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoveFilterSelection {
    filters: Vec<Filter>,
}

struct ButtonStr<'a>(usize, &'a Filter);

impl<'a> Choice<'a> for ButtonStr<'a> {
    type Action = Self;

    fn button(&self) -> Button<'a, Self::Action> {
        Button::Text {
            text: format!("Regel {}", self.0 + 1).into(),
            action: |x| x,
        }
    }
}

impl RemoveFilterSelection {
    fn buttons(&self) -> impl Choices<ButtonStr<'_>> {
        self.filters
            .iter()
            .enumerate()
            .map(|(x, y)| ButtonStr(x, y))
    }
    pub(super) async fn handle_message(
        self,
        cx: HandleMessage<'_>,
        channel: Option<SelectedChannel>,
    ) -> HandlerResult {
        let chat_id = cx.selected_chat(&channel).await?;

        match self.buttons().match_action(cx.message) {
            Some(ButtonStr(i, filter)) => {
                let removed = cx
                    .inner
                    .database
                    .update_filter(chat_id, &|filters| {
                        if filters[i] == *filter {
                            filters.remove(i);
                            true
                        } else {
                            false
                        }
                    })
                    .await?;

                let text = if removed {
                    "✅ Die Regel wurde gelöscht!"
                } else {
                    "❌ Die Regel konnte leider nicht gelöscht werden. Bitte versuche es erneut."
                };

                cx.reset_dialogue(channel).await?;
                respond!(cx, text, reply_markup = remove_keyboard()).await
            }
            None => {
                let text = format!(
                    "Bitte nutze die Schaltflächen, um einen Regel auszuwählen, oder sende /{} zum Abbrechen",
                    super::command_cancel::COMMAND.name
                );
                let reply_markup = self.buttons().keyboard_markup();
                respond!(cx, text, reply_markup).await
            }
        }
    }
}

pub async fn handle_command(cx: HandleMessage<'_>, _: Option<&str>) -> HandlerResult {
    let dialogue = cx.get_dialogue().await?;
    let chat_id = cx.selected_chat(&dialogue.channel).await?;
    let filters = cx.inner.database.get_filters(chat_id).await?;

    if filters.is_empty() {
        let target = SelectedChannel::chat_selection_accusative(&dialogue.channel);
        let (text, entities) =
            concat!("Zur Zeit sind keine Regeln für ", target, " aktiv!").to_message()?;
        return respond!(cx, text, entities, reply_markup = remove_keyboard()).await;
    }

    let mut msg = MessageBuilder::new();

    msg.write("Aktuelle Auswahl: ")?;
    msg.write(SelectedChannel::chat_selection(&dialogue.channel))?;
    msg.write("\n\nWähle einen der folgenden Regeln zum Löschen aus:\n\n")?;

    for (i, f) in filters.iter().enumerate() {
        msg.writeln(bold(concat!("Regel ", i + 1)))?;
        msg.writeln(f)?;
    }

    let (text, entities) = msg.build();
    let reply_markup = filters
        .iter()
        .enumerate()
        .map(|(x, y)| ButtonStr(x, y))
        .keyboard_markup();
    let state = RemoveFilterSelection { filters };

    cx.update_dialogue(state, dialogue.channel).await?;
    respond!(cx, text, entities, reply_markup).await
}
