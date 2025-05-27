use frankenstein::types::ChatMember;

pub mod broadcasting;
pub mod command;
pub mod response;
pub mod updates;

#[macro_export]
macro_rules! respond {
    (@param $m:expr; maybe_reply_parameters group) => {if $m.chat.id < 0{
        let p = ::frankenstein::types::ReplyParameters::builder().message_id($m.message_id).build();
        Some(p)
    } else {
        None
    }};
    (@param $m:expr; reply_parameters always) => {
        ::frankenstein::types::ReplyParameters::builder().message_id($m.message_id).build()
    };

    (@param $m:expr; $p:ident) => {$p};
    (@param $m:expr; $p:ident $v:expr) => {$v};
    ($bot:expr, $message:expr $(,$p:ident $(= $v:expr)?)* $(,)? ) => {{
        let thread_id = $message
            .is_topic_message
            .unwrap_or(false)
            .then_some($message.message_thread_id)
            .flatten();
        let params = ::frankenstein::methods::SendMessageParams::builder()
            .chat_id($message.chat.id)
            .maybe_message_thread_id(thread_id)
            $(.$p($crate::respond!(@param $message;$p $($v)?)))*
            .build();

        async move {
            ::frankenstein::AsyncTelegramApi::send_message($bot, &params).await.map(|_|())
        }
    }};
}

pub type ChatId = i64;

pub fn can_send_messages(c: &ChatMember) -> bool {
    match c {
        ChatMember::Administrator(member) => member.can_post_messages.unwrap_or(true),
        ChatMember::Restricted(member) => member.can_send_messages,
        ChatMember::Left(_) => false,
        ChatMember::Kicked(_) => false,
        _ => true,
    }
}
