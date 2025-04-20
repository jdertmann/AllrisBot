use std::fmt::Write;

use super::{Command, HandleMessage, HandlerResult, SelectedChannel};
use crate::bot::keyboard::remove_keyboard;
use crate::escape_html;

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

    let target = SelectedChannel::chat_selection_html_accusative(&dialogue.channel);

    let text = if filters.is_empty() {
        format!("Es sind keine Regeln für {target} aktiv.")
    } else {
        let mut text = format!("Zur Zeit sind die folgenden Regeln für {target} aktiv:\n\n");
        for (i, f) in filters.iter().enumerate() {
            let filter = escape_html(f.to_string());
            writeln!(&mut text, "<b>Regel {}</b>\n{filter}", i + 1).unwrap();
        }
        text
    };

    respond_html!(cx, text, reply_markup = remove_keyboard()).await
}
