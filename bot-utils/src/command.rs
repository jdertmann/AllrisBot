use std::fmt::Display;

use frankenstein::AsyncTelegramApi;
use frankenstein::methods::SetMyCommandsParams;
use frankenstein::types::{BotCommand, BotCommandScope};
use regex::{Regex, RegexBuilder, escape};

#[derive(Debug, Clone, Copy)]
pub struct ParsedCommand<'a> {
    pub command: &'a str,
    pub username: Option<&'a str>,
    pub param: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct CommandParser(Regex);

impl CommandParser {
    pub fn new(username: Option<&str>) -> Self {
        let pattern = if let Some(username) = username {
            &format!("^/([a-z0-9_]+)(?:@({}))?(?:\\s+(.*))?$", escape(username))
        } else {
            log::warn!("Bot has no username!");
            "^/([a-z0-9_]+)(?:@([a-z0-9_]+))?(?:\\s+(.*))?$"
        };

        let regex = RegexBuilder::new(pattern)
            .case_insensitive(true)
            .dot_matches_new_line(true)
            .build()
            .expect("regex should be valid!");

        Self(regex)
    }

    pub fn parse<'a>(&self, text: &'a str) -> Option<ParsedCommand<'a>> {
        self.0.captures(text).map(|captures| {
            let command = captures.get(1).expect("group matches always").as_str();
            let param;
            let username;
            if captures.len() == 4 {
                username = captures.get(2).map(|m| m.as_str());
                param = captures.get(3).map(|m| m.as_str());
            } else {
                username = None;
                param = captures.get(2).map(|m| m.as_str());
            }

            ParsedCommand {
                command,
                param,
                username,
            }
        })
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
    pub group_admin: bool,
    pub group_member: bool,
    pub private_chat: bool,
    pub admin: bool,
}

impl Command {
    fn params(&self) -> BotCommand {
        BotCommand::builder()
            .command(self.name)
            .description(self.description)
            .build()
    }
}

impl Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "/{} – {}", self.name, self.description)
    }
}

pub async fn set_my_commands<'a, B: AsyncTelegramApi>(
    bot: &B,
    scope: BotCommandScope,
    commands: impl IntoIterator<Item = &'a Command>,
) -> Result<(), B::Error> {
    let commands = commands.into_iter().map(Command::params).collect();
    let params = SetMyCommandsParams::builder()
        .scope(scope)
        .commands(commands)
        .build();

    bot.set_my_commands(&params).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_parser() {
        struct TestCase<'a> {
            desc: &'a str,
            bot_username: Option<&'a str>,
            input: &'a str,
            expected_command: Option<&'a str>,
            expected_username: Option<&'a str>,
            expected_param: Option<&'a str>,
        }

        let cases = [
            // Bot username provided
            TestCase {
                desc: "Match with username and param",
                bot_username: Some("my_bot"),
                input: "/start@my_bot hello",
                expected_command: Some("start"),
                expected_username: Some("my_bot"),
                expected_param: Some("hello"),
            },
            TestCase {
                desc: "Match with username and no param",
                bot_username: Some("my_bot"),
                input: "/start@my_bot",
                expected_command: Some("start"),
                expected_username: Some("my_bot"),
                expected_param: None,
            },
            TestCase {
                desc: "Match with no username and param",
                bot_username: Some("my_bot"),
                input: "/start hello",
                expected_command: Some("start"),
                expected_username: None,
                expected_param: Some("hello"),
            },
            TestCase {
                desc: "Mismatch username",
                bot_username: Some("my_bot"),
                input: "/start@wrong_bot hello",
                expected_command: None,
                expected_username: None,
                expected_param: None,
            },
            // Bot username not provided
            TestCase {
                desc: "Username missing, param given",
                bot_username: None,
                input: "/start hello",
                expected_command: Some("start"),
                expected_username: None,
                expected_param: Some("hello"),
            },
            TestCase {
                desc: "Username present, param given",
                bot_username: None,
                input: "/start@other_bot hello",
                expected_command: Some("start"),
                expected_username: Some("other_bot"),
                expected_param: Some("hello"),
            },
            TestCase {
                desc: "Username present, no param",
                bot_username: None,
                input: "/start@other_bot",
                expected_command: Some("start"),
                expected_username: Some("other_bot"),
                expected_param: None,
            },
            TestCase {
                desc: "No username, no param",
                bot_username: None,
                input: "/start",
                expected_command: Some("start"),
                expected_username: None,
                expected_param: None,
            },
        ];

        for case in &cases {
            let parser = CommandParser::new(case.bot_username);
            let result = parser.parse(case.input);

            match case.expected_command {
                Some(cmd) => {
                    let Some(parsed) = result else {
                        panic!("{}: Expected a match", case.desc)
                    };
                    assert_eq!(parsed.command, cmd, "{}: command mismatch", case.desc);
                    assert_eq!(
                        parsed.username, case.expected_username,
                        "{}: username mismatch",
                        case.desc
                    );
                    assert_eq!(
                        parsed.param, case.expected_param,
                        "{}: param mismatch",
                        case.desc
                    );
                }
                None => {
                    assert!(result.is_none(), "{}: Expected no match", case.desc);
                }
            }
        }
    }
}
