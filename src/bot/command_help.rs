use std::sync::OnceLock;

use frankenstein::types::MessageEntity;
use telegram_message_builder::{MessageBuilder, bold, concat, italic, text_link};

use super::{Command, HandleMessage, HandlerResult, command_privacy};
use crate::bot::{
    command_cancel, command_help, command_new_rule, command_remove_all_rules, command_remove_rule,
    command_rules, command_start, command_target,
};

pub const COMMAND: Command = Command {
    name: "hilfe",
    description: "Zeige die Hilfenachricht an",

    group_admin: true,
    group_member: true,
    private_chat: true,
    admin: true,
};

static MESSAGE_PRIVATE: OnceLock<(String, Vec<MessageEntity>)> = OnceLock::new();
static MESSAGE_GROUP: OnceLock<(String, Vec<MessageEntity>)> = OnceLock::new();

fn message(
    group: bool,
    owner: Option<&str>,
) -> Result<(String, Vec<MessageEntity>), telegram_message_builder::Error> {
    let mut msg = MessageBuilder::new();

    msg.pushln(concat!(
        bold("🤖 Allris-Bot"),
        "\nDieser Bot benachrichtigt dich, wenn im ",
        text_link("https://www.bonn.sitzung-online.de", "Ratsinformationssystem der Stadt Bonn"),
        " neue Vorlagen veröffentlicht werden – lege dazu ",
        bold("Regeln"),
        " fest, welche Vorlagen du erhalten willst.\n\n",

        bold("🔧 Regeln verwalten\n"),
        italic("Du erhältst Benachrichtungen für alle Vorlagen, auf die mindestens eine Regel zutrifft.\n"),
        command_new_rule::COMMAND,
        command_rules::COMMAND,
        command_remove_rule::COMMAND,
        command_remove_all_rules::COMMAND,
    ))?;

    if !group {
        msg.pushln(concat!(
            bold("📬 Ziel einstellen\n"),
            italic(
                "Der Bot kann Benachrichtigungen hier im Chat oder in einem deiner Kanäle senden.\n"
            ),
            command_target::COMMAND,
        ))?;
    }

    msg.push(concat!(
        bold("🆘 Sonstiges\n"),
        command_cancel::COMMAND,
        format_args!(
            "/{hilfe} oder /{start} – Zeige diese Hilfe an\n",
            hilfe = command_help::COMMAND.name,
            start = command_start::COMMAND.name,
        ),
        command_privacy::COMMAND,
        "\n",

        bold("📚 Reguläre Ausdrücke (Regex)"),
        "\nBeim Erstellen einer Regel kannst du festlegen, dass ein bestimmtes Merkmal ein sogenanntes Regex-Pattern erfüllen muss. ",
        "Gib dort einfach den Text ein, nach dem du filtern möchtest – das funktioniert in den meisten Fällen zuverlässig. ",
        "Falls du komplexere Muster brauchst, helfen dir ",
        text_link("https://regex101.com", "regex101.com"),
        " oder ChatGPT beim Ausprobieren und Erlernen von regulären Ausdrücken.\n\n",

        bold("👨‍💻 Mehr Infos & Kontakt"),
        "\nDer Quellcode dieses Bots ist öffentlich zugänglich: ",
        env!("CARGO_PKG_REPOSITORY"),
    ))?;

    if let Some(owner) = owner {
        msg.push("\n\nFragen, Feedback oder Ideen? Schreib mir gern: @")?;
        msg.push(owner)?;
    }

    Ok(msg.build())
}

pub async fn handle_command(cx: HandleMessage<'_>, _: Option<&str>) -> HandlerResult {
    let owner = cx.inner.owner.as_deref();
    let (text, entities) = if cx.chat_id() < 0 {
        MESSAGE_GROUP.get_or_init(|| message(true, owner).expect("help message too long!"))
    } else {
        MESSAGE_PRIVATE.get_or_init(|| message(false, owner).expect("help message too long!"))
    };
    respond!(cx, text, entities = entities.clone()).await
}
