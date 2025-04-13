use std::fmt::{Debug, Display};

use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use rand::distr::Alphanumeric;

#[derive(Debug)]
pub struct AdminToken {
    token: String,
    valid_until: DateTime<Utc>,
    used: bool,
}

impl AdminToken {
    pub fn new() -> Self {
        let token = rand::rng()
            .sample_iter(&Alphanumeric)
            .take(12)
            .map(char::from)
            .collect();

        let valid_until = Utc::now() + Duration::minutes(10);

        Self {
            token,
            valid_until,
            used: false,
        }
    }

    pub fn validate(&mut self, input: &str) -> bool {
        let valid = !self.used && Utc::now() < self.valid_until && input == self.token;

        if valid {
            self.used = true;
        }

        valid
    }
}

impl Display for AdminToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.token)
    }
}
