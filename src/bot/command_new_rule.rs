use std::convert::identity;
use std::iter;

use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use telegram_message_builder::{MessageBuilder, WriteToMessage, bold, code, concat, pre};

use super::keyboard::{force_reply, remove_keyboard};
use super::{Command, Error, SelectedChannel};
use crate::bot::keyboard::{Button, Choice, Choices};
use crate::bot::{HandleMessage, HandlerResult};
use crate::types::{Condition, Filter, Tag};

pub const COMMAND: Command = Command {
    name: "neue_regel",
    description: "Erstelle eine neue Benachrichtigungsregel",

    group_admin: true,
    group_member: true,
    private_chat: true,
    admin: true,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TagButton {
    Save,
    Select(Tag),
}

impl<'a> Choice<'a> for TagButton {
    type Action = TagButton;

    fn button(&self) -> Button<'a, Self> {
        match self {
            TagButton::Save => Button::Text {
                text: "✅ Speichern".into(),
                action: identity,
            },
            TagButton::Select(tag) => Button::Text {
                text: tag.label().into(),
                action: identity,
            },
        }
    }
}

fn buttons() -> Vec<TagButton> {
    Tag::TAGS
        .iter()
        .copied()
        .map(TagButton::Select)
        .chain(iter::once(TagButton::Save))
        .collect()
}

pub async fn handle_command(cx: HandleMessage<'_>, _: Option<&str>) -> HandlerResult {
    let dialogue = cx.get_dialogue().await?;

    let reply_markup = buttons().keyboard_markup();
    let (text, entities) = concat!(
        "🎛️ ",
        bold("Regel erstellen"),
        "\n\n\
        Wähle ein Merkmal für die erste Bedingung oder tippe auf „Speichern“, \
        um die Regel sofort ohne Bedingungen (alle Vorlagen werden erfasst) anzulegen.\n\n\
        Ausgewählter Chat: ",
        SelectedChannel::chat_selection(&dialogue.channel)
    )
    .to_message()?;

    cx.update_dialogue(TagSelection::default(), dialogue.channel)
        .await?;
    respond!(cx, text, entities, reply_markup).await
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagSelection {
    previous_conditions: Vec<Condition>,
}

impl TagSelection {
    pub(super) async fn handle_message(
        self,
        cx: HandleMessage<'_>,
        channel: Option<SelectedChannel>,
    ) -> HandlerResult {
        match buttons().match_action(cx.message) {
            Some(TagButton::Save) => {
                let chat_id = cx.selected_chat(&channel).await?;

                cx.inner
                    .database
                    .update_filter(chat_id, &|filters| {
                        filters.push(Filter {
                            conditions: self.previous_conditions.clone(),
                        });
                    })
                    .await?;

                let (text, entities) = concat!(
                    "✅ Die Regel für ",
                    SelectedChannel::chat_selection_accusative(&channel),
                    " wurde gespeichert und ist nun aktiv!"
                )
                .to_message()?;

                cx.reset_dialogue(channel).await?;

                respond!(cx, text, entities, reply_markup = remove_keyboard()).await
            }
            Some(TagButton::Select(tag)) => {
                let state = PatternInput {
                    previous_conditions: self.previous_conditions,
                    tag,
                };

                let mut msg = MessageBuilder::new();

                msg.push("Du hast das Merkmal ")?;
                msg.push(bold(tag.label()))?;
                if let Some(desc) = tag.description() {
                    msg.push(format_args!(" ({desc})"))?;
                }
                msg.push(" gewählt.")?;

                if !tag.examples().is_empty() {
                    msg.push("\n\nMögliche Werte sind beispielsweise: ")?;
                    for (i, example) in tag.examples().iter().enumerate() {
                        if i != 0 {
                            msg.push(", ")?;
                        }
                        msg.push(code(example))?;
                    }
                }

                msg.push("\n\nGib nun ein Regex-Pattern ein, wie z. B. ")?;
                msg.push(code("Wert"))?;
                msg.push(" oder ")?;
                msg.push(code("Option 1|Option 2"))?;
                msg.push(
                    ". Um die Bedingung \
                     umzudrehen, beginne mit einem Ausrufezeichen – dann werden \
                     alle Vorlagen, auf die das Pattern zutrifft, ausgeschlossen.",
                )?;

                let (text, entities) = msg.build();

                cx.update_dialogue(state, channel).await?;
                respond!(
                    cx,
                    text,
                    entities,
                    reply_markup = force_reply("Regex-Pattern")
                )
                .await
            }
            None => {
                let text = format!(
                    "️Bitte wähle ein gültiges Merkmal aus, oder sende /{} zum Abbrechen",
                    super::command_cancel::COMMAND.name
                );

                respond!(cx, text, reply_markup = buttons().keyboard_markup()).await
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatternInput {
    previous_conditions: Vec<Condition>,
    tag: Tag,
}

impl PatternInput {
    pub(super) async fn handle_message(
        self,
        cx: HandleMessage<'_>,
        channel: Option<SelectedChannel>,
    ) -> HandlerResult {
        let Some(text) = &cx.message.text else {
            return Err(Error::UnexpectedMessage);
        };

        let (negation, raw_pattern) = match text.strip_prefix('!') {
            Some(pat) => (true, pat),
            None => (false, text.as_str()),
        };

        let regex_check = if raw_pattern.contains('\n') {
            let text = "❌ Ungültiges Regex-Pattern: Zeilenumbrüche sind nicht erlaubt. Bitte versuche es erneut.".to_message()?;
            Err(text)
        } else if let Err(e) = RegexBuilder::new(raw_pattern).size_limit(10000).build() {
            let text = match e {
                regex::Error::CompiledTooBig(_) => {
                    "❌ Ungültiges Regex-Pattern: Das Pattern ist zu groß. Bitte versuche es erneut.".to_message()?
                }
                e => {
                    concat!(
                        "❌ Ungültiges Regex-Pattern. Bitte versuche es erneut. Tipp: Frage ChatGPT um Hilfe.\n\n",
                        pre(e)
                    ).to_message()?
                }
            };
            Err(text)
        } else {
            Ok(())
        };

        if let Err((text, entities)) = regex_check {
            respond!(
                cx,
                text,
                entities,
                reply_markup = force_reply("Regex-Pattern")
            )
            .await?;
            return Ok(());
        }

        let mut conditions = self.previous_conditions;
        conditions.push(Condition {
            tag: self.tag,
            pattern: raw_pattern.to_string(),
            negate: negation,
        });

        let summary = Filter { conditions };
        let text = format_args!(
            "Bedingung hinzugefügt – aktuelle Regel:\n\n{summary}\n\
            Wähle ein weiteres Merkmal oder tippe auf „Speichern“.",
        )
        .to_message()?
        .0;

        let state = TagSelection {
            previous_conditions: summary.conditions,
        };

        cx.update_dialogue(state, channel).await?;
        respond!(cx, text, reply_markup = buttons().keyboard_markup()).await
    }
}
