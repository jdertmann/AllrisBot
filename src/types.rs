use std::fmt::Display;

use regex::Regex;
use serde::{Deserialize, Serialize};
use teloxide::types::{InlineKeyboardButton, ParseMode};

pub type ChatId = i64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub text: String,
    pub parse_mode: ParseMode,
    pub buttons: Vec<InlineKeyboardButton>,
    pub tags: Vec<(Tag, String)>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Tag {
    Dsnr,
    Art,
    Gremium,
    Verfasser,
    Federführend,
    Beteiligt,
}

impl Tag {
    pub const TAGS: &'static [Self] = &[
        Tag::Dsnr,
        Tag::Art,
        Tag::Beteiligt,
        Tag::Federführend,
        Tag::Gremium,
        Tag::Verfasser,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Tag::Dsnr => "Drucksachen-Nummer",
            Tag::Art => "Art der Vorlage",
            Tag::Gremium => "Gremium",
            Tag::Verfasser => "Antrag- oder Fragesteller:in",
            Tag::Federführend => "Federführendes Amt",
            Tag::Beteiligt => "Beteiligtes Amt",
        }
    }

    pub fn description(&self) -> Option<&'static str> {
        match self {
            Tag::Dsnr => None,
            Tag::Art => None,
            Tag::Gremium => Some("Gremien, die zur Beratung der Vorlage vorgesehen sind."),
            Tag::Verfasser => {
                Some("Personen oder Fraktionen, die den Antrag oder die Frage gestellt haben.")
            }
            Tag::Federführend => None,
            Tag::Beteiligt => Some(
                "Jedes an der Vorlage beteiligte Amt. Das schließt auch das federführende Amt mit ein.",
            ),
        }
    }

    pub fn examples(&self) -> &'static [&'static str] {
        match self {
            Tag::Dsnr => &["252807", "242248-02 AA"],
            Tag::Art => &[
                "Beschlussvorlage, Stellungnahme der Verwaltung",
                "Anregungen und Beschwerden",
            ],
            Tag::Gremium => &[
                "Rat",
                "Bezirksvertretung Beuel",
                "Schulausschuss",
                "Städtebau- und Gestaltungsbeirat",
            ],
            Tag::Verfasser => &[
                "SPD-Fraktion im Rat der Stadt Bonn",
                "Patrick Tollasz",
                "CDU Bezirksfraktion Bad Godesberg",
            ],
            Tag::Federführend | Tag::Beteiligt => &[
                "Dezernat II",
                "52 Sport- und Bäderamt",
                "OB-22 Stabsstelle Bürgerbeteiligung",
                "61-3 Stadtverkehr",
            ],
        }
    }
}

mod serde_regex {
    use regex::Regex;
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &regex::Regex, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(value.as_str())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<regex::Regex, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = <&str>::deserialize(deserializer)?;
        Regex::new(s).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Condition {
    pub tag: Tag,
    #[serde(with = "serde_regex")]
    pub pattern: Regex,
    pub negate: bool,
}

impl Display for Condition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} passt {}zu \"{}\"",
            self.tag.label(),
            if self.negate { "nicht " } else { "" },
            self.pattern.as_str()
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Filter {
    pub conditions: Vec<Condition>,
}

impl Display for Filter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.conditions.is_empty() {
            writeln!(f, "Alle Vorlagen")?;
        } else {
            for (i, condition) in self.conditions.iter().enumerate() {
                let and = if i == 0 { "  " } else { "& " };
                writeln!(f, "{and}{condition}")?;
            }
        }

        Ok(())
    }
}
