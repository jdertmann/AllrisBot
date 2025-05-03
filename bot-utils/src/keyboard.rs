//! Provides an abstraction over reply keyboards.
//!
//! The api seems really awkward, but it works well in practice.

use std::borrow::Cow;

use frankenstein::types::{
    ForceReply, KeyboardButton, KeyboardButtonRequestChat, Message, ReplyKeyboardMarkup,
    ReplyKeyboardRemove, ReplyMarkup,
};

use crate::channel::SelectedChannel;

pub trait Choice<'a>: Sized {
    type Action: 'a;

    fn button(&self) -> Button<'a, Self>;
}

pub enum Button<'a, C: Choice<'a>> {
    Text {
        text: Cow<'a, str>,
        action: fn(C) -> C::Action,
    },
    RequestChat {
        text: Cow<'a, str>,
        request_id: i32,
        request_chat: fn(i32) -> KeyboardButtonRequestChat,
        action: fn(SelectedChannel) -> C::Action,
    },
}

impl<'a, C: Choice<'a>> Button<'a, C> {
    fn keyboard_button(&self) -> KeyboardButton {
        match self {
            Self::Text { text, .. } => KeyboardButton::builder().text(text.as_ref()).build(),
            Self::RequestChat {
                text,
                request_id,
                request_chat,
                ..
            } => KeyboardButton::builder()
                .text(text.as_ref())
                .request_chat(request_chat(*request_id))
                .build(),
        }
    }

    fn match_action(&self, option: C, msg: &Message) -> Option<C::Action> {
        match self {
            Self::Text { text, action } => {
                (msg.text.as_deref() == Some(text.as_ref())).then(|| action(option))
            }
            Self::RequestChat {
                request_id, action, ..
            } => {
                if let Some(chat_shared) = &msg.chat_shared {
                    (chat_shared.request_id == *request_id).then(|| {
                        let channel = SelectedChannel {
                            chat_id: chat_shared.chat_id,
                            title: chat_shared.title.clone(),
                            username: chat_shared.username.clone(),
                        };
                        action(channel)
                    })
                } else {
                    None
                }
            }
        }
    }
}

pub trait Choices<A> {
    fn match_action(self, message: &Message) -> Option<A>;

    fn keyboard_markup(self) -> ReplyMarkup;
}

impl<'a, B: Choice<'a>, T: IntoIterator<Item = B>> Choices<B::Action> for T {
    fn match_action(self, message: &Message) -> Option<B::Action> {
        self.into_iter()
            .find_map(|x| x.button().match_action(x, message))
    }

    fn keyboard_markup(self) -> ReplyMarkup {
        const BUTTONS_PER_ROW: usize = 2;
        let mut keyboard: Vec<Vec<KeyboardButton>> = vec![];
        for button in self {
            let b = button.button().keyboard_button();
            match keyboard.last_mut() {
                Some(x) if x.len() < BUTTONS_PER_ROW => x.push(b),
                _ => keyboard.push(vec![b]),
            }
        }

        let keyboard = ReplyKeyboardMarkup::builder()
            .keyboard(keyboard)
            .one_time_keyboard(true)
            .resize_keyboard(true)
            .build();

        ReplyMarkup::ReplyKeyboardMarkup(keyboard)
    }
}

pub fn remove_keyboard() -> ReplyMarkup {
    ReplyMarkup::ReplyKeyboardRemove(ReplyKeyboardRemove::builder().remove_keyboard(true).build())
}

pub fn force_reply(placeholder: &str) -> ReplyMarkup {
    ReplyMarkup::ForceReply(
        ForceReply::builder()
            .force_reply(true)
            .input_field_placeholder(placeholder)
            .selective(true)
            .build(),
    )
}
