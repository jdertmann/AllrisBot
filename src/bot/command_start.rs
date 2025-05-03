use bot_utils::Command;

use super::{HandleMessage, HandlerResult, command_help, command_privacy};

pub const COMMAND: Command = Command {
    name: "start",
    description: "Zeige die Hilfenachricht an",

    group_admin: true,
    group_member: true,
    private_chat: true,
    admin: true,
};

pub async fn handle_command(cx: HandleMessage<'_>, param: Option<&str>) -> HandlerResult {
    if param == Some(command_privacy::COMMAND.name) {
        command_privacy::handle_command(cx, None).await
    } else {
        command_help::handle_command(cx, param).await
    }
}
