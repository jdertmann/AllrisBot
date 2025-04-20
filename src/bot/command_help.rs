use std::sync::OnceLock;

use frankenstein::types::LinkPreviewOptions;

use super::{Command, HandleMessage, HandlerResult};

pub const COMMAND: Command = Command {
    name: "hilfe",
    description: "Zeige die Hilfenachricht an",

    group_admin: true,
    group_member: true,
    private_chat: true,
    admin: true,
};

macro_rules! command_text {
    ($x:ident) => {
        format_args!(
            "/{} – {}",
            super::$x::COMMAND.name,
            super::$x::COMMAND.description
        )
    };
}

static MESSAGE_PRIVATE: OnceLock<String> = OnceLock::new();
static MESSAGE_GROUP: OnceLock<String> = OnceLock::new();

fn message(group: bool, owner: Option<&str>) -> String {
    let ziel = if !group {
        &format!(
            "📬 <b>Ziel einstellen</b>\n\
             <i>Der Bot kann Benachrichtigungen hier im Chat oder in einem deiner Kanäle senden.</i>\n\
             {ziel}\n\n",
            ziel = command_text!(command_target)
        )
    } else {
        ""
    };

    let contact = if let Some(owner) = owner {
        &format!("\n\nFragen, Feedback oder Ideen? Schreib mir gern: @{owner}")
    } else {
        ""
    };

    format!("🤖 <b>Allris-Bot</b>
Dieser Bot benachrichtigt dich, wenn im <a href=\"https://www.bonn.sitzung-online.de/\">Ratsinformationssystem der Stadt Bonn</a> \
neue Vorlagen veröffentlicht werden – lege dazu <b>Regeln</b> fest, welche Vorlagen du erhalten willst.

🔧 <b>Regeln verwalten</b>
<i>Du erhältst Benachrichtungen für alle Vorlagen, auf die mindestens eine Regel zutrifft.</i>
{neue_regel}
{regeln}
{regel_loeschen}
{alle_regeln_loeschen}

{ziel}\
🆘 <b>Sonstiges</b>
{abbrechen}
/{hilfe} oder /{start} – Zeige diese Hilfe an

📚 <b>Reguläre Ausdrücke (Regex)</b>  
Beim Erstellen einer Regel kannst du festlegen, dass ein bestimmtes Merkmal ein sogenanntes Regex-Pattern erfüllen muss. \
Gib dort einfach den Text ein, nach dem du filtern möchtest – das funktioniert in den meisten Fällen zuverlässig.
Falls du komplexere Muster brauchst, \
helfen dir <a href=\"https://regex101.com\">regex101.com</a> oder ChatGPT beim Ausprobieren und Erlernen von regulären Ausdrücken.

👨‍💻 <b>Mehr Infos & Kontakt</b>  
Der Quellcode dieses Bots ist öffentlich zugänglich: https://github.com/jdertmann/allrisbot\
{contact}",
    neue_regel = command_text!(command_new_rule),
    regeln = command_text!(command_rules),
    regel_loeschen = command_text!(command_remove_rule),
    alle_regeln_loeschen = command_text!(command_remove_all_rules),
    abbrechen=command_text!(command_cancel),
    hilfe = super::command_help::COMMAND.name,
    start = super::command_start::COMMAND.name
)
}

pub async fn handle_command(cx: HandleMessage<'_>, _: Option<&str>) -> HandlerResult {
    let owner = cx.inner.owner.as_deref();
    let text = if cx.chat_id() < 0 {
        MESSAGE_GROUP.get_or_init(|| message(true, owner))
    } else {
        MESSAGE_PRIVATE.get_or_init(|| message(false, owner))
    };
    let link_preview_options = LinkPreviewOptions::builder().is_disabled(true).build();
    respond_html!(cx, text, link_preview_options).await
}
