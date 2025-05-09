//! Utility for constructing Telegram messages with rich text entities.
//!
//! This crate offers an ergonomic way to build richly formatted Telegram messages.
//! It supports all commonly used entity types and respects Telegram's message length
//! constraints.
//!
//! # Example
//!
//! ```
//! use frankenstein::types::MessageEntityType;
//! use telegram_message_builder::{bold, italic, concat, WriteToMessage};
//!
//! let (text, entities) = concat!("👋 ", bold("Hello"), " ", italic("world!"))
//!     .to_message()
//!     .unwrap();
//!
//! assert_eq!(text, "👋 Hello world!");
//! assert_eq!(entities[1].type_field, MessageEntityType::Italic);
//! assert_eq!(entities[1].offset, 9);
//! ```

use std::fmt::{Display, Write};

#[doc(no_inline)]
pub use frankenstein::types::MessageEntity;
use frankenstein::types::MessageEntityType;

/// The maximum Telegram message length in characters
pub const CHAR_LIMIT: usize = 4096;

/// Errors related to message construction
#[derive(Debug)]
pub enum Error {
    /// The total character count exceeded the character limit
    MessageTooLong,
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MessageTooLong => write!(f, "Message exceeds character limit"),
        }
    }
}

impl std::error::Error for Error {}

/// Builder for constructing Telegram messages with formatting
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct MessageBuilder {
    buf: String,
    entities: Vec<MessageEntity>,
    len_chars: usize,
    len_utf16: usize,
    char_limit: usize,
}

impl TryFrom<String> for MessageBuilder {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Error> {
        let len_chars = value.chars().count();
        if len_chars > CHAR_LIMIT {
            return Err(Error::MessageTooLong);
        }

        let len_utf16 = value.encode_utf16().count();

        Ok(MessageBuilder {
            buf: value,
            entities: vec![],
            len_chars,
            len_utf16,
            char_limit: CHAR_LIMIT,
        })
    }
}

impl Default for MessageBuilder {
    fn default() -> Self {
        Self::new()
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
            char_limit: CHAR_LIMIT,
        }
    }

    /// Sets a custom character limit for the message.
    ///
    /// This function allows you to specify a custom message length limit for the current `MessageBuilder`.
    /// If something is written that would cause the message to exceed this limit, the write operation will
    /// return [`Error::MessageTooLong`].
    ///
    /// Panics if the provided `limit` exceeds the global [`CHAR_LIMIT`].
    ///
    /// # Example
    ///
    /// This example demonstrates using `set_char_limit` with [`from_fn`] to generate a message from a list.
    /// The message will be truncated gracefully if it would exceed the limit:
    ///
    /// ```
    /// use telegram_message_builder::{
    ///     CHAR_LIMIT, Error, MessageBuilder,
    ///     WriteToMessage, bold, from_fn
    /// };
    ///
    /// let truncation_marker = "[...]";
    /// let long_list = &["item"; 5000];
    ///
    /// let list = from_fn(|msg| {
    ///     // Reserve space for the truncation marker
    ///     msg.set_char_limit(CHAR_LIMIT - truncation_marker.len());
    ///
    ///     let mut truncated = false;
    ///     for item in long_list {
    ///         if msg.writeln(item).is_err() {
    ///             truncated = true;
    ///             break;
    ///         }
    ///     }
    ///
    ///     // Restore full limit and add truncation marker, if necessary
    ///     msg.set_char_limit(CHAR_LIMIT);
    ///     if truncated {
    ///         msg.write(truncation_marker).unwrap();
    ///     }
    ///     Ok(())
    /// });
    ///
    /// let (text, entities) = list.to_message().unwrap();
    ///
    /// assert!(text.len() <= CHAR_LIMIT);
    /// assert!(text.ends_with(truncation_marker));
    /// ```
    pub fn set_char_limit(&mut self, limit: usize) {
        assert!(limit <= CHAR_LIMIT);
        self.char_limit = limit;
    }

    /// Returns the current message character limit.
    pub fn get_char_limit(&mut self) -> usize {
        self.char_limit
    }

    /// Returns the number of unicode characters in the message.
    pub fn len_chars(&self) -> usize {
        self.len_chars
    }

    /// Appends a [`WriteToMessage`] item.
    pub fn write(&mut self, s: impl WriteToMessage) -> Result<(), Error> {
        s.write_to(self)
    }

    /// Appends a [`WriteToMessage`] item followed by a newline (`\n`).
    pub fn writeln(&mut self, s: impl WriteToMessage) -> Result<(), Error> {
        self.write(s)?;
        self.write_str("\n")
    }

    /// Appends a [`Display`] item.
    pub fn write_fmt(&mut self, d: impl Display) -> Result<(), Error> {
        let current_len = self.buf.len();
        write!(&mut self.buf, "{d}").expect("writing to String never fails");

        let added = &self.buf[current_len..];
        let char_count = added.chars().count();

        if self.len_chars + char_count > self.char_limit {
            self.buf.truncate(current_len);
            return Err(Error::MessageTooLong);
        }

        self.len_chars += char_count;
        self.len_utf16 += added.encode_utf16().count();

        Ok(())
    }

    /// Appends a plain string.
    pub fn write_str(&mut self, s: &str) -> Result<(), Error> {
        let char_count = s.chars().count();

        if self.len_chars + char_count > self.char_limit {
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

impl WriteToMessage for Box<dyn WriteToMessage> {
    fn write_to(&self, message: &mut MessageBuilder) -> Result<(), Error> {
        (**self).write_to(message)
    }

    fn to_message(&self) -> Result<(String, Vec<MessageEntity>), Error> {
        (**self).to_message()
    }
}

impl<T: Display> WriteToMessage for T {
    fn write_to(&self, message: &mut MessageBuilder) -> Result<(), Error> {
        write!(message, "{self}")
    }

    fn to_message(&self) -> Result<(String, Vec<MessageEntity>), Error> {
        let msg = self.to_string();
        if msg.chars().count() > CHAR_LIMIT {
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
/// by one of the crate's helper functions like [`bold`] or [`italic`].
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

    /// Adds an expandable quote block.
    expandable_blockquote: ExpandableBlockquote;
}

/// Creates a custom emoji.
///
/// `alt` must be a valid single emoji as an alternative value for the custom emoji.
/// It will be shown instead of the custom emoji in places where a custom emoji
/// cannot be displayed or if the message is forwarded by a non-premium user.
/// It is recommended to use the emoji from the `emoji` field of the custom emoji sticker.
pub const fn custom_emoji<S: AsRef<str>, T: Display>(emoji_id: S, alt: T) -> WithEntity<T, S> {
    WithEntity {
        entity: EntityType::CustomEmoji(emoji_id),
        text: alt,
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
/// Useful for ad hoc formatting or builder-based generation. Note that if the closure returns
/// an error, the [`MessageBuilder`] will be reset to its previous state.
///
/// # Example
///
/// ```
/// use telegram_message_builder::{MessageBuilder, WriteToMessage, bold, concat, from_fn};
///
/// let items = &[
///     ("Milk", false),
///     ("", false),
///     ("Bread", true),
///     ("Eggs", false),
/// ];
///
/// let list = from_fn(|msg| {
///     for &(item, important) in items {
///         if item.trim().is_empty() {
///             continue;
///         }
///
///         if important {
///             msg.writeln(bold(item))?;
///         } else {
///             msg.writeln(item)?;
///         }
///     }
///     Ok(())
/// });
///
/// let (text, entities) = concat!("Shopping List:\n", list).to_message().unwrap();
///
/// assert_eq!(text.trim(), "Shopping List:\nMilk\nBread\nEggs");
/// assert_eq!(entities.len(), 1);
/// assert_eq!(entities[0].offset, 20);
/// assert_eq!(entities[0].length, 5);
/// ```
pub const fn from_fn<F: Fn(&mut MessageBuilder) -> Result<(), Error>>(f: F) -> impl WriteToMessage {
    struct FromFn<F>(F);

    impl<F: Fn(&mut MessageBuilder) -> Result<(), Error>> WriteToMessage for FromFn<F> {
        fn write_to(&self, message: &mut MessageBuilder) -> Result<(), Error> {
            let old_len = message.buf.len();
            let old_len_entities = message.entities.len();
            let old_len_chars = message.len_chars;
            let old_len_utf16 = message.len_utf16;
            let old_limit = message.char_limit;

            let result = self.0(message);

            if result.is_err() {
                // reset everything to the original state
                message.buf.truncate(old_len);
                message.entities.truncate(old_len_entities);
                message.len_chars = old_len_chars;
                message.len_utf16 = old_len_utf16;
                message.char_limit = old_limit;
            }

            result
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
    use std::fmt::Debug;
    use std::hint::black_box;

    use super::{concat, *};

    fn get_entity(entities: &Vec<MessageEntity>) -> &MessageEntity {
        entities.first().expect("expected at least one entity")
    }

    #[test]
    fn test_simple_push() {
        let mut builder = MessageBuilder::new();
        builder.write_str("Hello, world!").unwrap();
        assert_eq!(builder.as_str(), "Hello, world!");
    }

    #[test]
    fn test_entity_bold() {
        let (text, entities) = bold("bold text").to_message().unwrap();
        assert_eq!(text, "bold text");
        let entity = get_entity(&entities);
        assert_eq!(entity.offset, 0);
        assert_eq!(entity.length, 9);
        assert_eq!(entity.type_field, MessageEntityType::Bold);
    }

    #[test]
    fn test_text_link() {
        let (text, entities) = text_link("https://example.com", "click here")
            .to_message()
            .unwrap();
        assert_eq!(text, "click here");
        let entity = get_entity(&entities);
        assert_eq!(entity.offset, 0);
        assert_eq!(entity.length, 10);
        assert_eq!(entity.type_field, MessageEntityType::TextLink);
        assert_eq!(entity.url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn test_pre_with_language() {
        let (text, entities) = pre_with_language("rust", "fn main() {}")
            .to_message()
            .unwrap();
        assert_eq!(text, "fn main() {}");
        let entity = get_entity(&entities);
        assert_eq!(entity.type_field, MessageEntityType::Pre);
        assert_eq!(entity.language.as_deref(), Some("rust"));
    }

    #[test]
    fn test_concat_macro() {
        let (text, entities) = concat!(bold("Hello "), italic("world"))
            .to_message()
            .unwrap();
        assert_eq!(text, "Hello world");
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].type_field, MessageEntityType::Bold);
        assert_eq!(entities[1].type_field, MessageEntityType::Italic);
        assert_eq!(entities[1].offset, 6);
    }

    #[test]
    fn test_utf16_length() {
        let mut builder = MessageBuilder::new();
        builder.write_str("abc😀").unwrap();
        builder.write_str("💡").unwrap();

        assert_eq!(builder.len_utf16, 7);
        assert_eq!(builder.len_chars, 5);
    }

    #[test]
    fn test_message_length_limit() {
        let long_str = "a".repeat(CHAR_LIMIT + 1);
        let result = MessageBuilder::new().write_str(&long_str);
        assert!(matches!(result, Err(Error::MessageTooLong)));

        let long_str = "😀".repeat(CHAR_LIMIT);
        MessageBuilder::new()
            .write_str(&long_str)
            .expect("CHAR_LIMIT characters are ok");
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
    fn test_entity_offsets_with_unicode() {
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

    #[test]
    fn test_custom_char_limit() {
        use crate::{CHAR_LIMIT, MessageBuilder};

        let mut msg = MessageBuilder::default();

        assert_eq!(msg.get_char_limit(), CHAR_LIMIT);
        msg.set_char_limit(10);
        assert_eq!(msg.get_char_limit(), 10);

        assert!(msg.write_str("12345").is_ok());
        assert!(msg.write_fmt(67).is_ok());
        assert_eq!(msg.as_str(), "1234567");

        let mut msg = MessageBuilder::new();
        assert_eq!(msg.get_char_limit(), CHAR_LIMIT);

        msg.set_char_limit(6);
        assert!(msg.write_str("abc").is_ok());
        assert!(write!(msg, "123").is_ok());
        assert!(msg.write_str("!").is_err());
        assert_eq!(msg.as_str(), "abc123");

        let mut msg = MessageBuilder::try_from(String::new()).unwrap();
        assert_eq!(msg.get_char_limit(), CHAR_LIMIT);

        msg.set_char_limit(4);
        assert!(msg.write_str("hello").is_err());
        assert_eq!(msg.as_str(), "");
    }

    #[test]
    #[should_panic]
    fn test_char_limit_too_high() {
        let mut msg = MessageBuilder::default();
        msg.set_char_limit(CHAR_LIMIT + 1);
    }

    #[test]
    fn test_reset_message_builder() {
        use crate::{MessageBuilder, WriteToMessage, from_fn};

        let initial_text = "👋 Hello, world!";
        let next_text = "Lorem ipsum ✅";
        let overflow_text = "This is a long message that will exceed the limit!";

        let mut msg = MessageBuilder::default();
        msg.set_char_limit(30);

        msg.write(initial_text).unwrap();

        let old_msg = msg.clone();

        let from_fn_example = from_fn(move |message| {
            message.write(bold(next_text))?;
            message.write(overflow_text)
        });

        let result = from_fn_example.write_to(&mut msg);

        assert!(result.is_err());
        assert_eq!(old_msg, msg);
    }

    #[test]
    fn test_error_on_write_fmt() {
        let mut msg = MessageBuilder::try_from(String::from("Test")).unwrap();
        msg.buf.shrink_to_fit();
        msg.set_char_limit(50);
        let old_msg = msg.clone();
        let old_capacity = msg.buf.capacity();

        write!(msg, "{:?}", black_box::<&dyn Debug>(&[i32::MAX; 10])).unwrap_err();

        assert_eq!(msg, old_msg);
        assert!(msg.buf.capacity() > old_capacity);
    }
}
