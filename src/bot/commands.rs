use frankenstein::AsyncTelegramApi;
use frankenstein::types::{
    ChatAdministratorRights, KeyboardButton, KeyboardButtonRequestChat, ReplyKeyboardMarkup,
    ReplyMarkup,
};

use super::{Error, HandleMessage, HandlerResult};
use crate::bot::DialogueState;

macro_rules! response_params {
    ($this:expr) => {
        ::frankenstein::methods::SendMessageParams::builder()
            .chat_id($this.chat_id())
            .maybe_message_thread_id($this.message.message_thread_id)
    };
}

impl HandleMessage<'_> {
    pub(super) async fn handle_command(self, cmd: &str, param: Option<&str>) -> HandlerResult {
        if self.message.message_thread_id.is_some() {
            return Err(Error::TopicsNotSupported);
        }

        let cmd = cmd.to_ascii_lowercase();

        macro_rules! cmds {
            ($($cmd:ident $( ($param:expr) )?),+) => {
                match cmd.as_str() {
                    $(stringify!($cmd) => self.$cmd($($param)?).await,)+
                    _ => Err(Error::UnknownCommand(cmd))
                }
            };
        }

        cmds! {
            selectchannel,
            start,
            help,
            cancel,
            addfilter,
            admin(param.ok_or(Error::UnknownCommand(cmd))?)
        }
    }

    async fn admin(self, token: &str) -> HandlerResult {
        if let Some(t) = &self.inner.admin_token {
            if t.lock().unwrap().validate(token) && self.chat_id() > 0 {
                if let Some(current_admin) = self.inner.database.get_admin().await? {
                    /*let params = SendMessageParams::builder()
                    .chat_id(&current_admin)
                    .text("Du bist kein Admin mehr")
                    .build();*/
                    //self.inner.bot.send_message(&params)?;
                }

                let user_id = self
                    .chat_id()
                    .try_into()
                    .expect("We checked that the value is > 0");

                self.inner.database.set_admin(user_id).await?;
                self.respond("Ok").await?;
                return Ok(());
            }
        }

        Err(Error::Unauthorized(self.chat_id(), "admin".into()))
    }

    async fn help(self) -> HandlerResult {
        todo!()
    }

    async fn start(self) -> HandlerResult {
        todo!()
    }

    async fn cancel(self) -> HandlerResult {
        self.with_dialogue(async |mut dialogue| {
            if matches!(dialogue.state, DialogueState::Initial) {
            } else {
            }

            dialogue.state = Default::default();

            Ok(dialogue)
        })
        .await
    }

    async fn addfilter(self) -> HandlerResult {
        todo!()
    }

    async fn selectchannel(self) -> HandlerResult {
        let request_id = self.message.message_id;

        self.with_dialogue(async move |mut d| {
            d.state = DialogueState::ReceiveChannelSelection { request_id };
            Ok(d)
        })
        .await?;

        let permissions = ChatAdministratorRights::builder()
            .is_anonymous(false)
            .can_manage_chat(false)
            .can_delete_messages(false)
            .can_restrict_members(false)
            .can_promote_members(false)
            .can_change_info(false)
            .can_invite_users(false)
            .can_manage_video_chats(false)
            .can_post_messages(true)
            .build();

        let button2 = KeyboardButtonRequestChat::builder()
            .request_id(request_id)
            .chat_is_channel(true)
            .user_administrator_rights(permissions)
            .bot_administrator_rights(permissions)
            .request_title(true)
            .build();
        let button1 = KeyboardButton::builder().text("Zurücksetzen").build();
        let button2 = KeyboardButton::builder()
            .text("Channel auswählen")
            .request_chat(button2)
            .build();
        let keyboard = ReplyKeyboardMarkup::builder()
            .keyboard(vec![vec![button1, button2]])
            .one_time_keyboard(true)
            .resize_keyboard(true)
            .build();
        let params = response_params!(self)
            .text(
                "Du kannst nun einen Channel auswählen, den du bearbeiten möchtest, \
                 oder du kannst die Einstellung zurücksetzen, um Änderungen für diesen \
                 Chat vorzunehmen.",
            )
            .reply_markup(ReplyMarkup::ReplyKeyboardMarkup(keyboard))
            .build();

        self.inner.bot.send_message(&params).await?;

        Ok(())
    }
}
