use frankenstein::ParseMode;

use super::{Command, HandleMessage, HandlerResult};

pub const COMMAND: Command = Command {
    name: "datenschutz",
    description: "Zeige die Datenschutzerklärung an",

    private_chat: true,
    group_member: true,
    group_admin: true,
    admin: true,
};

const TEXT: &str = include_str!("privacy.html");

pub async fn handle_command(cx: HandleMessage<'_>, _: Option<&str>) -> HandlerResult {
    let mut text = String::from(TEXT);
    if let Some(owner) = &cx.inner.owner {
        text += "\nBei Fragen kontaktiere mich direkt über Telegram: @";
        text += owner;
    }
    respond!(cx, text, parse_mode = ParseMode::Html).await
}
