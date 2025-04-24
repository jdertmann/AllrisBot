use super::Command;

pub const COMMAND: Command = Command {
    name: "start",
    description: "Zeige die Hilfenachricht an",

    group_admin: true,
    group_member: true,
    private_chat: true,
    admin: true,
};

pub use super::command_help::handle_command;
