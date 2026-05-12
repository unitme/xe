use std::{
    collections::VecDeque,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MediaKeyCode};

use crate::{
    models::{CliArgs, PlaybackMode, PlaylistFile, persist_resolved_id},
    player::MpvPlayer,
    youtube::YoutubeService,
};

const STATUS_LOG_CAP: usize = 8;

#[derive(Debug)]
pub struct App {
    pub playlist: PlaylistFile,
    pub selected: usize,
    pub playing_index: Option<usize>,
    pub started_at: Option<Instant>,
    pub paused: bool,
    pub volume: u8,
    pub muted: bool,
    pub mode: PlaybackMode,
    pub status: String,
    pub status_log: VecDeque<String>,
    rng_state: u64,
}

impl App {
    pub fn new(cli: CliArgs, mut playlist: PlaylistFile) -> Self {
        let max_index = playlist.entries.len().saturating_sub(1);
        let selected = cli.start_index.min(max_index);
        let resolved_count = playlist
            .entries
            .iter()
            .filter(|entry| entry.resolved_id.is_some())
            .count();
        let status = format!(
            "loaded '{}' ({} entries, {} pinned ids)",
            playlist.name,
            playlist.entries.len(),
            resolved_count
        );

        let mut status_log = VecDeque::with_capacity(STATUS_LOG_CAP);
        status_log.push_back(status.clone());
        playlist.name = playlist.name.trim().to_string();

        Self {
            playlist,
            selected,
            playing_index: None,
            started_at: None,
            paused: false,
            volume: 70,
            muted: false,
            mode: cli.mode,
            status,
            status_log,
            rng_state: seed_rng(),
        }
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        let status = status.into();
        self.status = status.clone();
        if self
            .status_log
            .back()
            .map(|previous| previous == &status)
            .unwrap_or(false)
        {
            return;
        }
        if self.status_log.len() == STATUS_LOG_CAP {
            self.status_log.pop_front();
        }
        self.status_log.push_back(status);
    }

    fn next_random_index(&mut self, exclude: Option<usize>) -> usize {
        let len = self.playlist.entries.len();
        if len <= 1 {
            return 0;
        }

        loop {
            self.rng_state ^= self.rng_state << 13;
            self.rng_state ^= self.rng_state >> 7;
            self.rng_state ^= self.rng_state << 17;
            let index = (self.rng_state as usize) % len;
            if Some(index) != exclude {
                return index;
            }
        }
    }

    fn advance_index(&mut self) -> Option<usize> {
        let current = self.playing_index.unwrap_or(self.selected);
        let len = self.playlist.entries.len();
        match self.mode {
            PlaybackMode::Sequential => (current + 1 < len).then_some(current + 1),
            PlaybackMode::Random => Some(self.next_random_index(Some(current))),
            PlaybackMode::LoopPlaylist => Some((current + 1) % len),
            PlaybackMode::LoopSong => Some(current),
        }
    }
}

pub async fn handle_key(
    key: KeyEvent,
    app: &mut App,
    youtube: &YoutubeService,
    player: &mut Option<MpvPlayer>,
    redraw: &mut dyn FnMut(&App) -> Result<()>,
) {
    let result = match key.code {
        KeyCode::Up => {
            app.selected = app.selected.saturating_sub(1);
            Ok(())
        }
        KeyCode::Down => {
            app.selected = (app.selected + 1).min(app.playlist.entries.len().saturating_sub(1));
            Ok(())
        }
        KeyCode::Enter => play_selected(app, youtube, player, redraw).await,
        KeyCode::Char(' ') | KeyCode::Char('p') => toggle_pause(app, youtube, player, redraw).await,
        KeyCode::Char('n') | KeyCode::Right => play_next(app, youtube, player, redraw).await,
        KeyCode::Char('b') | KeyCode::Left => play_previous(app, youtube, player, redraw).await,
        KeyCode::Char('+') | KeyCode::Char('=') => {
            set_volume(app, player, app.volume.saturating_add(5).min(100), redraw).await;
            Ok(())
        }
        KeyCode::Char('-') => {
            set_volume(app, player, app.volume.saturating_sub(5), redraw).await;
            Ok(())
        }
        KeyCode::Char('m') => {
            app.mode = app.mode.next();
            report(app, redraw, format!("mode {}", app.mode));
            Ok(())
        }
        KeyCode::Char('s') => stop(app, player, redraw).await,
        KeyCode::Media(media) => handle_media_key(media, app, youtube, player, redraw).await,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Ok(()),
        _ => Ok(()),
    };

    if let Err(error) = result {
        report(app, redraw, clean_error(error));
    }
}

pub async fn maybe_auto_advance(
    app: &mut App,
    youtube: &YoutubeService,
    player: &mut Option<MpvPlayer>,
    redraw: &mut dyn FnMut(&App) -> Result<()>,
) {
    if app.paused || app.playing_index.is_none() {
        return;
    }
    if app
        .started_at
        .map(|started| started.elapsed() < Duration::from_secs(3))
        .unwrap_or(true)
    {
        return;
    }

    let idle = match player.as_ref() {
        Some(player) => player.is_idle().await.unwrap_or(false),
        None => false,
    };
    if !idle {
        return;
    }

    let Some(next_index) = app.advance_index() else {
        app.playing_index = None;
        app.started_at = None;
        report(app, redraw, "end of playlist");
        return;
    };

    if let Err(error) = play_index(app, next_index, youtube, player, redraw).await {
        app.playing_index = None;
        app.started_at = None;
        report(app, redraw, clean_error(error));
    }
}

pub fn is_quit_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q'))
        || (matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL))
}

async fn handle_media_key(
    media: MediaKeyCode,
    app: &mut App,
    youtube: &YoutubeService,
    player: &mut Option<MpvPlayer>,
    redraw: &mut dyn FnMut(&App) -> Result<()>,
) -> Result<()> {
    match media {
        MediaKeyCode::Play | MediaKeyCode::Pause | MediaKeyCode::PlayPause => {
            toggle_pause(app, youtube, player, redraw).await
        }
        MediaKeyCode::TrackNext | MediaKeyCode::FastForward => {
            play_next(app, youtube, player, redraw).await
        }
        MediaKeyCode::TrackPrevious | MediaKeyCode::Rewind => {
            play_previous(app, youtube, player, redraw).await
        }
        MediaKeyCode::RaiseVolume => {
            set_volume(app, player, app.volume.saturating_add(5).min(100), redraw).await;
            Ok(())
        }
        MediaKeyCode::LowerVolume => {
            set_volume(app, player, app.volume.saturating_sub(5), redraw).await;
            Ok(())
        }
        MediaKeyCode::MuteVolume => {
            app.muted = !app.muted;
            if let Some(player) = player.as_ref() {
                player.set_mute(app.muted).await?;
            }
            let status = if app.muted { "muted" } else { "unmuted" };
            report(app, redraw, status);
            Ok(())
        }
        MediaKeyCode::Stop => stop(app, player, redraw).await,
        _ => Ok(()),
    }
}

async fn toggle_pause(
    app: &mut App,
    youtube: &YoutubeService,
    player: &mut Option<MpvPlayer>,
    redraw: &mut dyn FnMut(&App) -> Result<()>,
) -> Result<()> {
    if app.playing_index.is_none() {
        return play_selected(app, youtube, player, redraw).await;
    }

    let mpv = ensure_player(player).await?;
    app.paused = !app.paused;
    mpv.set_pause(app.paused).await?;
    let status = if app.paused { "paused" } else { "playing" };
    report(app, redraw, status);
    Ok(())
}

async fn play_selected(
    app: &mut App,
    youtube: &YoutubeService,
    player: &mut Option<MpvPlayer>,
    redraw: &mut dyn FnMut(&App) -> Result<()>,
) -> Result<()> {
    play_index(app, app.selected, youtube, player, redraw).await
}

async fn play_next(
    app: &mut App,
    youtube: &YoutubeService,
    player: &mut Option<MpvPlayer>,
    redraw: &mut dyn FnMut(&App) -> Result<()>,
) -> Result<()> {
    let current = app.playing_index.unwrap_or(app.selected);
    let next = match app.mode {
        PlaybackMode::Sequential | PlaybackMode::LoopPlaylist => {
            if current + 1 < app.playlist.entries.len() {
                current + 1
            } else if app.mode == PlaybackMode::LoopPlaylist {
                0
            } else {
                bail!("end of playlist");
            }
        }
        PlaybackMode::Random => app.next_random_index(Some(current)),
        PlaybackMode::LoopSong => current,
    };
    play_index(app, next, youtube, player, redraw).await
}

async fn play_previous(
    app: &mut App,
    youtube: &YoutubeService,
    player: &mut Option<MpvPlayer>,
    redraw: &mut dyn FnMut(&App) -> Result<()>,
) -> Result<()> {
    let current = app.playing_index.unwrap_or(app.selected);
    let previous = if current > 0 {
        current - 1
    } else if matches!(app.mode, PlaybackMode::LoopPlaylist) {
        app.playlist.entries.len().saturating_sub(1)
    } else {
        0
    };
    play_index(app, previous, youtube, player, redraw).await
}

async fn play_index(
    app: &mut App,
    index: usize,
    youtube: &YoutubeService,
    player: &mut Option<MpvPlayer>,
    redraw: &mut dyn FnMut(&App) -> Result<()>,
) -> Result<()> {
    let (query, line, existing_id) = {
        let entry = app
            .playlist
            .entries
            .get(index)
            .ok_or_else(|| anyhow!("selected track is outside the playlist"))?;
        (entry.query.clone(), entry.source_line, entry.resolved_id)
    };

    let id = match existing_id {
        Some(id) => {
            report(app, redraw, format!("using pinned id for {query}"));
            id
        }
        None => {
            report(app, redraw, format!("searching YouTube for {query}"));
            let found = youtube.search_best_video_id(&query).await?;
            persist_resolved_id(&app.playlist.path, line, &query, found)?;
            if let Some(entry) = app.playlist.entries.get_mut(index) {
                entry.resolved_id = Some(found);
            }
            report(
                app,
                redraw,
                format!("pinned {} in playlist file", found.as_str()),
            );
            found
        }
    };

    report(app, redraw, format!("resolving stream for {}", id.as_str()));
    let stream = youtube.resolve_stream(id).await?;

    let volume = app.volume;
    let muted = app.muted;
    report(app, redraw, "starting mpv");
    let mpv = ensure_player(player).await?;
    mpv.load(&stream, volume, muted).await?;

    app.selected = index;
    app.playing_index = Some(index);
    app.started_at = Some(Instant::now());
    app.paused = false;
    report(app, redraw, format!("playing {query}"));
    Ok(())
}

async fn stop(
    app: &mut App,
    player: &mut Option<MpvPlayer>,
    redraw: &mut dyn FnMut(&App) -> Result<()>,
) -> Result<()> {
    if let Some(player) = player.as_ref() {
        player.stop().await?;
    }
    app.playing_index = None;
    app.started_at = None;
    app.paused = false;
    report(app, redraw, "stopped");
    Ok(())
}

async fn set_volume(
    app: &mut App,
    player: &mut Option<MpvPlayer>,
    volume: u8,
    redraw: &mut dyn FnMut(&App) -> Result<()>,
) {
    app.volume = volume;
    if let Some(player) = player.as_ref()
        && let Err(error) = player.set_volume(volume).await
    {
        report(app, redraw, clean_error(error));
        return;
    }
    report(app, redraw, format!("volume {volume}"));
}

async fn ensure_player(player: &mut Option<MpvPlayer>) -> Result<&MpvPlayer> {
    if player.is_none() {
        *player = Some(MpvPlayer::spawn().await?);
    }
    player
        .as_ref()
        .ok_or_else(|| anyhow!("mpv player unavailable"))
}

fn report(app: &mut App, redraw: &mut dyn FnMut(&App) -> Result<()>, status: impl Into<String>) {
    app.set_status(status);
    let _ = redraw(app);
}

pub(crate) fn clean_error(error: anyhow::Error) -> String {
    let message = error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" | ");
    if message.is_empty() {
        "unknown error".to_string()
    } else {
        message.replace(['\n', '\r', '\t'], " ")
    }
}

fn seed_rng() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}
