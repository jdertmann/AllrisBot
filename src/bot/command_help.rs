use std::sync::OnceLock;

use frankenstein::types::MessageEntity;
use telegram_message_builder::{WriteToMessage, bold, concat, from_fn, italic, text_link};

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

const fn intro_paragraph() -> impl WriteToMessage {
    concat!(
        bold("🤖 Allris-Bot"),
        "\nDieser Bot benachrichtigt dich, wenn im ",
        text_link(
            "https://www.bonn.sitzung-online.de",
            "Ratsinformationssystem der Stadt Bonn"
        ),
        " neue Vorlagen veröffentlicht werden – lege dazu ",
        bold("Regeln"),
        " fest, welche Vorlagen du erhalten willst.\n"
    )
}

const fn rules_paragraph() -> impl WriteToMessage {
    let desc =
        "Du erhältst Benachrichtungen für alle Vorlagen, auf die mindestens eine Regel zutrifft.";
    concat!(
        bold("🔧 Regeln verwalten"),
        "\n",
        italic(desc),
        "\n",
        command_new_rule::COMMAND,
        command_rules::COMMAND,
        command_remove_rule::COMMAND,
        command_remove_all_rules::COMMAND,
    )
}

const fn target_paragraph() -> impl WriteToMessage {
    let desc = "Der Bot kann Benachrichtigungen hier im Chat oder in einem deiner Kanäle senden.";
    concat!(
        bold("📬 Ziel einstellen"),
        "\n",
        italic(desc),
        "\n",
        command_target::COMMAND,
    )
}

fn miscellaneous_paragraph() -> impl WriteToMessage {
    from_fn(|msg| {
        msg.writeln(bold("🆘 Sonstiges"))?;

        write!(
            msg,
            "{cancel}\
             /{hilfe} oder /{start} – Zeige diese Hilfe an\n\
             {privacy}",
            cancel = command_cancel::COMMAND,
            hilfe = command_help::COMMAND.name,
            start = command_start::COMMAND.name,
            privacy = command_privacy::COMMAND,
        )
    })
}

fn regex_paragraph() -> impl WriteToMessage {
    concat!(
        bold("📚 Reguläre Ausdrücke (Regex)"),
        "\nBeim Erstellen einer Regel kannst du festlegen, dass ein bestimmtes Merkmal ein sogenanntes Regex-Pattern erfüllen muss. \
         Gib dort einfach den Text ein, nach dem du filtern möchtest – das funktioniert in den meisten Fällen zuverlässig. \
         Falls du komplexere Muster brauchst, helfen dir ",
        text_link("https://regex101.com", "regex101.com"),
        " oder ChatGPT beim Ausprobieren und Erlernen von regulären Ausdrücken.\n"
    )
}

fn disclaimer_paragraph() -> impl WriteToMessage {
    concat!(
        bold("⚖️ Hinweis"),
        "\nDieser Bot ist ein rein privates, nicht-kommerzielles Projekt zur \
        automatischen Benachrichtigung über neue Vorlagen aus dem ALLRIS®-System der Stadt Bonn. \
        Er steht weder in Verbindung zur Firma CC e-gov GmbH noch zur Stadt Bonn. \n",
        bold(
            "Für Vollständigkeit, Richtigkeit oder Aktualität der bereitgestellten \
            Informationen wird keine Gewähr übernommen.",
        ),
        "\n",
    )
}

fn about_paragraph(owner: Option<&str>) -> impl WriteToMessage {
    from_fn(move |msg| {
        msg.writeln(bold("👨‍💻 Mehr Infos & Kontakt"))?;

        msg.write(concat!(
            "Der Quellcode dieses Bots steht unter der Lizenz",
            text_link(
                "https://www.gnu.org/licenses/agpl-3.0.html.en",
                "GNU AGPL v3"
            ),
            " zur Verfügung: "
        ))?;
        write!(
            msg,
            "{} (Version {})",
            env!("CARGO_PKG_REPOSITORY"),
            env!("CARGO_PKG_VERSION")
        )?;

        if let Some(owner) = owner {
            write!(
                msg,
                "\n\nFragen, Feedback oder Ideen? Schreib mir gern: @{owner}"
            )?;
        }

        Ok(())
    })
}

fn message(group: bool, owner: Option<&str>) -> (String, Vec<MessageEntity>) {
    from_fn(|msg| {
        msg.writeln(intro_paragraph())?;
        msg.writeln(rules_paragraph())?;

        if !group {
            msg.writeln(target_paragraph())?;
        }

        msg.writeln(miscellaneous_paragraph())?;
        msg.writeln(regex_paragraph())?;
        msg.writeln(disclaimer_paragraph())?;
        msg.write(about_paragraph(owner))
    })
    .to_message()
    .expect("help message too long!")
}

pub async fn handle_command(cx: HandleMessage<'_>, _: Option<&str>) -> HandlerResult {
    let owner = cx.inner.owner.as_deref();
    let (text, entities) = if cx.chat_id() < 0 {
        MESSAGE_GROUP.get_or_init(|| message(true, owner))
    } else {
        MESSAGE_PRIVATE.get_or_init(|| message(false, owner))
    };
    respond!(cx, text, entities = entities.clone()).await
}
