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
cd AllrisBot/allrisbot
cargo build --release
```

### 3. Usage

See `./target/release/allrisbot --help` for usage details.

## Contributing

If you’d like to make contributions, feel free to open an issue or pull request.

## License (`allrisbot` crate)

Copyright (C) 2025 Johannes Dertmann

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU Affero General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU Affero General Public License for more details.

You should have received a copy of the GNU Affero General Public License
along with this program.  If not, see <http://www.gnu.org/licenses/>.
