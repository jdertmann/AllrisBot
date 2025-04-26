use super::keyboard::remove_keyboard;
use super::{Command, DialogueState, HandleMessage, HandlerResult};

pub const COMMAND: Command = Command {
    name: "abbrechen",
    description: "Brich den aktuellen Vorgang ab",

    group_admin: true,
    group_member: true,
    private_chat: true,
    admin: true,
};

pub async fn handle_command(cx: HandleMessage<'_>, _: Option<&str>) -> HandlerResult {
    let dialogue = cx.get_dialogue().await?;

    let text = if dialogue.state != DialogueState::default() {
        cx.reset_dialogue(dialogue.channel).await?;
        "Befehl wurde abgebrochen!"
    } else {
        "Es war kein Befehl aktiv"
    };

    respond!(cx, text, reply_markup = remove_keyboard()).await
}
