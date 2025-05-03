#[cfg(feature = "broadcasting")]
pub mod broadcasting;
pub mod channel;
mod command;
pub mod keyboard;
#[cfg(feature = "get-updates")]
mod updates;
use frankenstein::types::ChatMember;

pub use crate::command::*;
#[cfg(feature = "get-updates")]
pub use crate::updates::*;
pub type ChatId = i64;

pub fn can_send_messages(member: &ChatMember) -> bool {
    match member {
        ChatMember::Administrator(member) => member.can_post_messages.unwrap_or(true),
        ChatMember::Restricted(member) => member.can_send_messages,
        ChatMember::Left(_) => false,
        ChatMember::Kicked(_) => false,
        _ => true,
    }
}

#[macro_export]
macro_rules! respond {
    (@param $p:ident) => {$p};
    (@param $p:ident $v:expr) => {$v};
    ($bot:expr, $message:expr $(,$p:ident $(= $v:expr)?)* $(,)? ) => {{
        let reply_parameters = if $message.chat.id < 0 {
            let p = ::frankenstein::types::ReplyParameters::builder().message_id($message.message_id).build();
            Some(p)
        } else {
            None
        };
        let thread_id = $message
            .is_topic_message
            .unwrap_or(false)
            .then_some($message.message_thread_id)
            .flatten();
        let params = ::frankenstein::methods::SendMessageParams::builder()
            .chat_id($message.chat.id)
            .maybe_message_thread_id(thread_id)
            .maybe_reply_parameters(reply_parameters)
            $(.$p($crate::respond!(@param $p $($v)?)))*
            .build();

        async move {
            ::frankenstein::AsyncTelegramApi::send_message($bot, &params).await.map(|_|())
        }
    }};
}
