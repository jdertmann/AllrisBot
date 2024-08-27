use std::collections::HashSet;
use std::sync::Arc;

use chrono::prelude::*;
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use teloxide::utils::command::BotCommands;
use teloxide::utils::html;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration, MissedTickBehavior};

mod feed;

const FEED_URL: &str = "https://www.bonn.sitzung-online.de/rss/voreleased";

async fn feed_updater(bot: Bot, users: Arc<Mutex<HashSet<ChatId>>>) {
    let client = reqwest::Client::new();
    let pattern = regex::RegexBuilder::new("</h3>.*<h3>([^<]*)</h3>")
        .dot_matches_new_line(true)
        .build()
        .unwrap();

    let mut interval = interval(Duration::from_secs(300));
    let mut saved: Option<(NaiveDate, HashSet<String>)> = None;

    interval.set_missed_tick_behavior(MissedTickBehavior::Delay); // not that it will probably happen

    loop {
        interval.tick().await;
        log::info!("Doing update");

        let channel = match feed::fetch_feed(&client, FEED_URL).await {
            Ok(channel) => channel,
            Err(e) => {
                log::error!("Failed to retrieve feed: {e}");
                continue;
            }
        };

        let date = channel.pub_date.date_naive();

        if let Some((old_date, known_guids)) = &mut saved {
            if *old_date != date {
                // neuer Tag, neues Glück
                known_guids.clear()
            }

            for item in channel.item {
                if !known_guids.insert(item.guid) {
                    // item already known
                    continue;
                }
                let title = match pattern
                    .captures(&item.description)
                    .map(|m| m.get(1))
                    .flatten()
                {
                    Some(m) => m.as_str(),
                    None => continue,
                };

                let msg = html::bold(title)
                    + &html::escape("\n\n👉 ")
                    + &html::link(&item.link, &html::escape("Zur Vorlage"));

                let users = users.lock().await;
                for user in users.iter() {
                    let request = bot.send_message(*user, &msg).parse_mode(ParseMode::Html);

                    if let Err(e) = request.await {
                        log::warn!("Sending notification failed: {e}");
                        // TODO: Maybe retry or remove user from list
                    }
                }
            }

            *old_date = date;
        } else {
            let known_guids = channel.item.into_iter().map(|x| x.guid).collect();
            saved = Some((date, known_guids))
        }
    }
}

#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "lowercase",
    description = "Diese Befehle werden unterstützt:"
)]
enum Command {
    #[command(description = "für Benachrichtigungen registrieren.")]
    Start,
    #[command(description = "Benachrichtigungen abbestellen.")]
    Stop,
    #[command(description = "zeige diesen Text.")]
    Help,
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let bot = Bot::from_env();
    let registered_users: Arc<Mutex<HashSet<ChatId>>> = Arc::new(Mutex::new(HashSet::new()));

    tokio::spawn(feed_updater(bot.clone(), Arc::clone(&registered_users)));

    let answer = move |bot: Bot, msg: Message, cmd: Command| {
        let registered_users = Arc::clone(&registered_users);
        async move {
            match cmd {
                Command::Start => {
                    let mut users = registered_users.lock().await;
                    let added = users.insert(msg.chat.id);
                    let reply = if added {
                        "Du hast dich erfolgreich für Benachrichtigungen registriert."
                    } else {
                        "Du bist bereits für Benachrichtigungen registriert."
                    };
                    bot.send_message(msg.chat.id, reply).await?;
                }
                Command::Stop => {
                    let mut users = registered_users.lock().await;
                    let removed = users.remove(&msg.chat.id);
                    let reply = if removed {
                        "Du hast die Benachrichtigungen abbestellt."
                    } else {
                        "Du warst nicht für Benachrichtigungen registriert."
                    };
                    bot.send_message(msg.chat.id, reply).await?;
                }
                Command::Help => {
                    bot.send_message(msg.chat.id, Command::descriptions().to_string())
                        .await?;
                }
            };
            Ok(())
        }
    };

    Command::repl(bot, answer).await;
}
