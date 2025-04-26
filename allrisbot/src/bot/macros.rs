macro_rules! respond {
    (@param $p:ident) => {$p};
    (@param $p:ident $v:expr) => {$v};
    ($this:expr $(,$p:ident $(= $v:expr)?)* $(,)? ) => {{
        let reply_parameters = if $this.chat_id() < 0 {
            let p = ::frankenstein::types::ReplyParameters::builder().message_id($this.message.message_id).build();
            Some(p)
        } else {
            None
        };
        let params = ::frankenstein::methods::SendMessageParams::builder()
            .chat_id($this.chat_id())
            .maybe_message_thread_id($this.message.message_thread_id)
            .maybe_reply_parameters(reply_parameters)
            $(.$p(respond!(@param $p $($v)?)))*
            .build();

        async move {
            ::frankenstein::AsyncTelegramApi::send_message(&$this.inner.bot, &params).await?;
            crate::bot::HandlerResult::Ok(())
        }
    }};
}

macro_rules! respond_html {
    ($this:expr $(,$p:ident $(= $v:expr)?)* $(,)?) => {
        respond!($this, parse_mode = ::frankenstein::ParseMode::Html $(, $p $(= $v)?)*)
    };
}
