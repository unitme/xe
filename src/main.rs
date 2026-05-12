use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEventKind};
use reqwest::Client;

mod app;
mod models;
mod player;
mod terminal;
mod ui;
mod youtube;

use app::{App, handle_key, is_quit_key, maybe_auto_advance};
use models::{load_playlist_file, parse_cli_args};
use player::MpvPlayer;
use terminal::TerminalGuard;
use youtube::YoutubeService;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = parse_cli_args(std::env::args())?;
    let playlist = load_playlist_file(&cli.playlist_path)?;

    let mut terminal = TerminalGuard::enter()?;
    let http = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to create HTTP client")?;

    let mut app = App::new(cli, playlist);
    let youtube = YoutubeService::new(http);
    let mut player: Option<MpvPlayer> = None;
    let mut last_idle_check = Instant::now();

    loop {
        terminal
            .terminal_mut()
            .draw(|frame| ui::draw(frame, &app))
            .context("failed to draw terminal UI")?;

        if last_idle_check.elapsed() >= Duration::from_millis(500) {
            last_idle_check = Instant::now();
            maybe_auto_advance(&mut app, &youtube, &mut player, &mut |app| {
                terminal
                    .terminal_mut()
                    .draw(|frame| ui::draw(frame, app))
                    .context("failed to draw terminal UI")?;
                Ok(())
            })
            .await;
        }

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        let key = match event::read()? {
            Event::Key(key) => key,
            _ => continue,
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if is_quit_key(key) {
            break;
        }

        handle_key(key, &mut app, &youtube, &mut player, &mut |app| {
            terminal
                .terminal_mut()
                .draw(|frame| ui::draw(frame, app))
                .context("failed to draw terminal UI")?;
            Ok(())
        })
        .await;
    }

    Ok(())
}
