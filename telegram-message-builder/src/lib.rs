//! Utility for constructing Telegram messages with rich text entities.
//!
//! This crate offers a type-safe, ergonomic way to build richly formatted Telegram messages
//! using a builder pattern. It respects Telegram's formatting rules and message length constraints,
//! and supports all commonly used entity types (bold, italic, code, links, etc.).
//!
//! # Examples
//!
//! ```
//! use frankenstein::types::MessageEntityType;
//! use telegram_message_builder::{bold, italic, concat, WriteToMessage};
//!
//! let (text, entities) = concat!("👋 ", bold("Hello"), " ", italic("world!")).to_message().unwrap();
//!
//! assert_eq!(text, "👋 Hello world!");
//! assert_eq!(entities[1].type_field, MessageEntityType::Italic);
//! assert_eq!(entities[1].offset, 9);
//! ```

use std::fmt::{Display, Write};

#[doc(no_inline)]
pub use frankenstein::types::MessageEntity;
use frankenstein::types::MessageEntityType;

/// Errors related to message construction
#[derive(Debug)]
pub enum Error {
    /// The total character count exceeded Telegram's 4096 character limit
    MessageTooLong,
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MessageTooLong => write!(f, "message exceeds limit of 4096 characters"),
        }
    }
}

impl std::error::Error for Error {}

/// Builder for constructing Telegram messages with formatting
#[derive(Default, Debug)]
pub struct MessageBuilder {
    buf: String,
    entities: Vec<MessageEntity>,
    len_chars: usize,
    len_utf16: usize,
}

impl TryFrom<String> for MessageBuilder {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Error> {
        let len_chars = value.chars().count();
        if len_chars > 4096 {
            return Err(Error::MessageTooLong);
        }

        let len_utf16 = value.encode_utf16().count();

        Ok(MessageBuilder {
            buf: value,
            entities: vec![],
            len_chars,
            len_utf16,
        })
    }
}

impl MessageBuilder {
    /// Creates a new, empty message builder.
    pub const fn new() -> Self {
        Self {
            buf: String::new(),
            entities: Vec::new(),
            len_chars: 0,
            len_utf16: 0,
        }
    }

    /// Returns the number of unicode characters in the message.
    pub fn len_chars(&self) -> usize {
        self.len_chars
    }

    /// Appends a [`WriteToMessage`] item.
    pub fn push(&mut self, s: impl WriteToMessage) -> Result<(), Error> {
        s.write_to(self)
    }

    /// Appends a [`WriteToMessage`] item followed by a newline (`\n`).
    pub fn pushln(&mut self, s: impl WriteToMessage) -> Result<(), Error> {
        self.push(s)?;
        self.push_str("\n")
    }

    /// Appends a [`Display`] item.
    fn push_display(&mut self, d: &impl Display) -> Result<(), Error> {
        let current_len = self.buf.len();
        write!(&mut self.buf, "{d}").expect("writing to String never fails");

        let added = &self.buf[current_len..];
        let char_count = added.chars().count();

        if self.len_chars + char_count > 4096 {
            self.buf.drain(current_len..);
            return Err(Error::MessageTooLong);
        }

        self.len_chars += char_count;
        self.len_utf16 += added.encode_utf16().count();

        Ok(())
    }

    /// Appends a plain string.
    pub fn push_str(&mut self, s: &str) -> Result<(), Error> {
        let char_count = s.chars().count();

        if self.len_chars + char_count > 4096 {
            return Err(Error::MessageTooLong);
        }

        self.len_chars += char_count;
        self.len_utf16 += s.encode_utf16().count();
        self.buf.push_str(s);

        Ok(())
    }

    /// Returns accumulated entities
    pub fn entities(&self) -> &[MessageEntity] {
        &self.entities
    }

    /// Returns the current text of the message.
    pub fn as_str(&self) -> &str {
        &self.buf
    }

    /// Consumes the builder and returns message and accumulated entities
    pub fn build(self) -> (String, Vec<MessageEntity>) {
        (self.buf, self.entities)
    }
}

/// Trait representing types that can be written into a [`MessageBuilder`], including rich text formatting.
///
/// Most commonly, you will not need to implement this trait manually. In particular, all items implementing
/// the [`Display`] trait also implement this trait. Formatting functions like [`bold`] or [`italic`] will also return
/// a type that implements `WriteToMessage`.
///
/// To create a custom implementation of this trait, consider using [`from_fn`].
pub trait WriteToMessage {
    /// Write the item into a [`MessageBuilder`]
    fn write_to(&self, message: &mut MessageBuilder) -> Result<(), Error>;

    /// Convert the item into its plain text content and a `Vec` of entities.
    fn to_message(&self) -> Result<(String, Vec<MessageEntity>), Error> {
        let mut msg = MessageBuilder::new();
        self.write_to(&mut msg)?;
        Ok(msg.build())
    }
}

impl WriteToMessage for &dyn WriteToMessage {
    fn write_to(&self, message: &mut MessageBuilder) -> Result<(), Error> {
        (*self).write_to(message)
    }
}

impl<T: Display> WriteToMessage for T {
    fn write_to(&self, message: &mut MessageBuilder) -> Result<(), Error> {
        message.push_display(self)
    }

    fn to_message(&self) -> Result<(String, Vec<MessageEntity>), Error> {
        let msg = self.to_string();
        if msg.chars().count() > 4096 {
            return Err(Error::MessageTooLong);
        }
        Ok((msg, vec![]))
    }
}

/// Entity variants supported by Telegram
#[repr(u8)]
enum EntityType<S> {
    Bold = 7,
    Italic = 8,
    Underline = 9,
    Strikethrough = 10,
    Spoiler = 11,
    Code = 12,
    Pre(Option<S>) = 13,
    TextLink(S) = 14,
    CustomEmoji(S) = 16,
    Blockquote = 17,
    ExpandableBlockquote = 18,
}

impl<S: AsRef<str>> EntityType<S> {
    fn create_entity(&self, offset: usize, len: usize) -> MessageEntity {
        let entity_type = match self {
            EntityType::Italic => MessageEntityType::Italic,
            EntityType::Bold => MessageEntityType::Bold,
            EntityType::Strikethrough => MessageEntityType::Strikethrough,
            EntityType::Underline => MessageEntityType::Underline,
            EntityType::Spoiler => MessageEntityType::Spoiler,
            EntityType::TextLink(_) => MessageEntityType::TextLink,
            EntityType::Code => MessageEntityType::Code,
            EntityType::Pre(_) => MessageEntityType::Pre,
            EntityType::CustomEmoji(_) => MessageEntityType::CustomEmoji,
            EntityType::Blockquote => MessageEntityType::Blockquote,
            EntityType::ExpandableBlockquote => MessageEntityType::ExpandableBlockquote,
        };

        let url = match self {
            EntityType::TextLink(url) => Some(url.as_ref()),
            _ => None,
        };

        let language = match self {
            EntityType::Pre(lang) => lang.as_ref().map(AsRef::as_ref),
            _ => None,
        };

        let emoji_id = match self {
            EntityType::CustomEmoji(id) => Some(id.as_ref()),
            _ => None,
        };

        MessageEntity::builder()
            .offset(offset as u16)
            .length(len as u16)
            .maybe_language(language)
            .maybe_url(url)
            .maybe_custom_emoji_id(emoji_id)
            .type_field(entity_type)
            .build()
    }
}

/// Wraps an inner writer with an entity tag.
///
/// It is usually not dealt with directly but instead created
/// by one of the crate's helper functions like [`bold`], [`italic`]
pub struct WithEntity<I, S = String> {
    text: I,
    entity: EntityType<S>,
}

impl<I: WriteToMessage, S: AsRef<str>> WriteToMessage for WithEntity<I, S> {
    fn write_to(&self, message: &mut MessageBuilder) -> Result<(), Error> {
        let pos1 = message.len_utf16;
        self.text.write_to(message)?;
        let pos2 = message.len_utf16;

        if pos2 > pos1 {
            let entity = self.entity.create_entity(pos1, pos2 - pos1);
            message.entities.push(entity);
        }

        Ok(())
    }
}

impl<I: WriteToMessage, S: AsRef<str>> WriteToMessage for &WithEntity<I, S> {
    fn write_to(&self, message: &mut MessageBuilder) -> Result<(), Error> {
        (*self).write_to(message)
    }
}

macro_rules! entity_fns {
    ($( $(#[$attr:meta])* $name:ident: $entity:ident;)*) => {$(
        $(#[$attr])*
        pub const fn $name<T: WriteToMessage>(text: T) -> WithEntity<T> {
            WithEntity {
                entity: EntityType::$entity,
                text,
            }
        }
    )*};
}

entity_fns! {
    /// Makes text bold.
    bold: Bold;

    /// Makes text italic.
    italic: Italic;

    /// Adds a strikethrough effect.
    strikethrough: Strikethrough;

    /// Hides text behind a spoiler. Tap to reveal.
    spoiler: Spoiler;

    /// Adds an underline.
    underline: Underline;

    /// Formats text as inline code.
    code: Code;

    /// Formats text as a quoted block.
    blockquote: Blockquote;

    /// Adds a collapsible quote block.
    expandable_blockquote: ExpandableBlockquote;
}

pub const fn custom_emoji<S: AsRef<str>, T: Display>(
    emoji_id: S,
    alt_emoji: T,
) -> WithEntity<T, S> {
    WithEntity {
        entity: EntityType::CustomEmoji(emoji_id),
        text: alt_emoji,
    }
}

/// Creates an inline text link.
pub const fn text_link<T: WriteToMessage, S: AsRef<str>>(url: S, text: T) -> WithEntity<T, S> {
    WithEntity {
        entity: EntityType::TextLink(url),
        text,
    }
}

/// Mentions a user by ID as inline link without using a username.
///
/// This is subject to restrictions (see [API docs](https://core.telegram.org/bots/api#formatting-options))
pub fn text_mention<T: WriteToMessage>(user_id: i64, text: T) -> WithEntity<T> {
    WithEntity {
        entity: EntityType::TextLink(format!("tg://user?id={user_id}")),
        text,
    }
}

/// Formats text as preformatted code block
pub const fn pre<T: WriteToMessage>(text: T) -> WithEntity<T> {
    WithEntity {
        entity: EntityType::Pre(None),
        text,
    }
}

/// Formats text as preformatted block with language (for syntax highlighting).
///
/// Telegram supports a variety of languages,
/// see [libprisma's supported languages](https://github.com/TelegramMessenger/libprisma?tab=readme-ov-file#supported-languages).
pub const fn pre_with_language<T: WriteToMessage, S: AsRef<str>>(
    language: S,
    text: T,
) -> WithEntity<T, S> {
    WithEntity {
        entity: EntityType::Pre(Some(language)),
        text,
    }
}

/// Converts a closure into a [`WriteToMessage`] implementation.
///
/// Useful for ad hoc formatting or builder-based generation.
pub const fn from_fn<F: Fn(&mut MessageBuilder) -> Result<(), Error>>(f: F) -> impl WriteToMessage {
    struct FromFn<F>(F);

    impl<F: Fn(&mut MessageBuilder) -> Result<(), Error>> WriteToMessage for FromFn<F> {
        fn write_to(&self, message: &mut MessageBuilder) -> Result<(), Error> {
            self.0(message)
        }
    }

    FromFn(f)
}

#[doc(hidden)]
pub use telegram_message_builder_macro::__concat_helper;

/// Macro for concatenating multiple [`WriteToMessage`] items into a single one.
///
/// Returns another `impl WriteToMessage`.
#[macro_export]
macro_rules! concat {
    ($($x:expr),* $(,)?) => {
        $crate::__concat_helper!($crate, $($x),* )
    };
}

#[cfg(test)]
mod tests {
    use super::{concat, *};

    fn get_text(msg: &(String, Vec<MessageEntity>)) -> &str {
        &msg.0
    }

    fn get_entity(msg: &(String, Vec<MessageEntity>)) -> &MessageEntity {
        msg.1.first().expect("expected at least one entity")
    }

    #[test]
    fn test_simple_push() {
        let mut builder = MessageBuilder::new();
        builder.push_str("Hello, world!").unwrap();
        assert_eq!(builder.as_str(), "Hello, world!");
    }

    #[test]
    fn test_entity_bold() {
        let message = bold("bold text").to_message().unwrap();
        assert_eq!(get_text(&message), "bold text");
        let entity = get_entity(&message);
        assert_eq!(entity.offset, 0);
        assert_eq!(entity.length, 9);
        assert_eq!(entity.type_field, MessageEntityType::Bold);
    }

    #[test]
    fn test_text_link() {
        let message = text_link("https://example.com", "click here")
            .to_message()
            .unwrap();
        assert_eq!(get_text(&message), "click here");
        let entity = get_entity(&message);
        assert_eq!(entity.offset, 0);
        assert_eq!(entity.length, 10);
        assert_eq!(entity.type_field, MessageEntityType::TextLink);
        assert_eq!(entity.url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn test_pre_with_language() {
        let message = pre_with_language("rust", "fn main() {}")
            .to_message()
            .unwrap();
        assert_eq!(get_text(&message), "fn main() {}");
        let entity = get_entity(&message);
        assert_eq!(entity.type_field, MessageEntityType::Pre);
        assert_eq!(entity.language.as_deref(), Some("rust"));
    }

    #[test]
    fn test_concat_macro() {
        let combined = concat!(bold("Hello "), italic("world"))
            .to_message()
            .unwrap();
        assert_eq!(get_text(&combined), "Hello world");
        assert_eq!(combined.1.len(), 2);
        assert_eq!(combined.1[0].type_field, MessageEntityType::Bold);
        assert_eq!(combined.1[1].type_field, MessageEntityType::Italic);
    }

    #[test]
    fn test_message_too_long() {
        let long_str = "a".repeat(4097);
        let result = MessageBuilder::new().push_str(&long_str);
        assert!(matches!(result, Err(Error::MessageTooLong)));

        let long_str = "😀".repeat(4096);
        MessageBuilder::new()
            .push_str(&long_str)
            .expect("4096 characters are ok");
    }

    #[test]
    fn test_utf16_accumulation_multiple_adds() {
        let mut builder = MessageBuilder::new();
        builder.push_str("abc😀").unwrap(); // "abc" + emoji
        builder.push_str("💡").unwrap(); // another emoji

        // "abc" = 3 UTF-16 units
        // 😀 and 💡 = 2 UTF-16 units each
        assert_eq!(builder.len_utf16, 7);
        assert_eq!(builder.len_chars(), 5); // 3 normal + 2 emoji
    }

    #[test]
    fn test_custom_emoji() {
        fn _impl_write_to_message<S: AsRef<str>, T: Display>(x: S, y: T) -> impl WriteToMessage {
            custom_emoji(x, y)
        }

        let (text, entities) = concat!(
            custom_emoji("emoji_id", '🍊'),
            custom_emoji("emoji_id", "👨‍💻")
        )
        .to_message()
        .unwrap();

        assert_eq!(entities.len(), 2);
        assert_eq!(&text, "🍊👨‍💻");
    }

    #[test]
    fn test_entity_offsets_with_combined_unicode() {
        let (text, entities) = concat!(
            bold(concat!(
                "abc😀 ",
                underline(" "),
                code(text_link("https://google.de", "")) // empty entities not counted
            )),
            italic("💡 test")
        )
        .to_message()
        .unwrap();

        assert_eq!(text, "abc😀  💡 test");
        assert_eq!(entities.len(), 3);

        let underline_entity = &entities[0];
        let italic_entity = &entities[2];

        let underline_start = "abc😀 ".encode_utf16().count();
        let underline_len = 1;
        let italic_start = "abc😀  ".encode_utf16().count();
        let italic_len = "💡 test".trim_end().encode_utf16().count();

        assert_eq!(underline_entity.offset as usize, underline_start);
        assert_eq!(underline_entity.length as usize, underline_len);
        assert_eq!(italic_entity.offset as usize, italic_start);
        assert_eq!(italic_entity.length as usize, italic_len);
    }
}
