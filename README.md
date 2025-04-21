# Allris Bot

A Telegram bot that notifies users about newly published documents in local council information systems powered by **Allris 4** and the **OParl API**. This bot was originally developed for the city of Bonn’s council information system, but should work with any Allris 4 instance or, with slight modifications, even other systems that implement the OParl standard.


Currently running as [@AllrisBot](https://t.me/AllrisBot) for Bonn.

## Getting Started

### 1. Requirements

- [Rust](https://www.rust-lang.org/tools/install)
- [Valkey](https://valkey.io/download/) as truly open-source redis alternative
- A **Telegram bot token** from [@BotFather](https://t.me/BotFather)

### 2. Installation

Clone the repository and build the project:

```bash
git clone https://github.com/jdertmann/AllrisBot.git
cd AllrisBot
cargo build --release
```

### 3. Usage

See `./target/release/allrisbot --help` for usage details.