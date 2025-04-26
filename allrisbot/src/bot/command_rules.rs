use telegram_message_builder::{MessageBuilder, WriteToMessage, bold, concat};

use super::{Command, HandleMessage, HandlerResult, SelectedChannel};
use crate::bot::keyboard::remove_keyboard;

pub const COMMAND: Command = Command {
    name: "regeln",
    description: "Zeige alle bestehenden Regeln an",

    group_admin: true,
    group_member: true,
    private_chat: true,
    admin: true,
};

pub async fn handle_command(cx: HandleMessage<'_>, _: Option<&str>) -> HandlerResult {
    let dialogue = cx.get_dialogue().await?;
    let chat_id = cx.selected_chat(&dialogue.channel).await?;
    let filters = cx.inner.database.get_filters(chat_id).await?;

    let target = SelectedChannel::chat_selection_accusative(&dialogue.channel);

    let (text, entities) = if filters.is_empty() {
        concat!("Es sind keine Regeln für ", target, " aktiv.").to_message()?
    } else {
        let mut message = MessageBuilder::new();

        message.push("Zur Zeit sind die folgenden Regeln für ")?;
        message.push(target)?;
        message.push(" aktiv:\n\n")?;

        for (i, f) in filters.iter().enumerate() {
            message.pushln(bold(concat!("Regel ", i + 1)))?;
            message.pushln(f)?;
        }

        message.build()
    };

    respond!(cx, text, entities, reply_markup = remove_keyboard()).await
}
