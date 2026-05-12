use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use url::Url;
use liber::YtId;

const ID_MARKER: &str = "# yt:";

#[derive(Debug, Clone)]
pub struct CliArgs {
    pub playlist_path: PathBuf,
    pub start_index: usize,
    pub mode: PlaybackMode,
}

#[derive(Debug, Clone)]
pub struct PlaylistFile {
    pub path: PathBuf,
    pub name: String,
    pub entries: Vec<PlaylistEntry>,
}

#[derive(Debug, Clone)]
pub struct PlaylistEntry {
    pub source_line: usize,
    pub query: String,
    pub resolved_id: Option<YtId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackMode {
    Sequential,
    Random,
    LoopPlaylist,
    LoopSong,
}

impl PlaybackMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "sequential" | "seq" => Some(Self::Sequential),
            "random" | "rand" => Some(Self::Random),
            "loop-playlist" | "playlist-loop" | "loop" => Some(Self::LoopPlaylist),
            "loop-song" | "song-loop" | "single" => Some(Self::LoopSong),
            _ => None,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Sequential => Self::Random,
            Self::Random => Self::LoopPlaylist,
            Self::LoopPlaylist => Self::LoopSong,
            Self::LoopSong => Self::Sequential,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Random => "random",
            Self::LoopPlaylist => "loop-playlist",
            Self::LoopSong => "loop-song",
        }
    }
}

impl fmt::Display for PlaybackMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

pub fn parse_cli_args(args: impl IntoIterator<Item = String>) -> Result<CliArgs> {
    let mut args = args.into_iter();
    let bin = args.next().unwrap_or_else(|| "play".to_string());

    let mut playlist_path = None;
    let mut start_index = 0usize;
    let mut mode = PlaybackMode::Sequential;

    let mut iter = args.peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                bail!(usage(&bin));
            }
            "--start" => {
                let value = iter
                    .next()
                    .ok_or_else(|| anyhow!("--start requires a 1-based index"))?;
                start_index = parse_start(&value)?;
            }
            "--mode" => {
                let value = iter
                    .next()
                    .ok_or_else(|| anyhow!("--mode requires a value"))?;
                mode = PlaybackMode::parse(&value).ok_or_else(|| {
                    anyhow!(
                        "unsupported mode '{value}', expected sequential|random|loop-playlist|loop-song"
                    )
                })?;
            }
            value if value.starts_with("--start=") => {
                start_index =
                    parse_start(value.split_once('=').map(|(_, v)| v).unwrap_or_default())?;
            }
            value if value.starts_with("--mode=") => {
                let mode_value = value.split_once('=').map(|(_, v)| v).unwrap_or_default();
                mode = PlaybackMode::parse(mode_value).ok_or_else(|| {
                    anyhow!(
                        "unsupported mode '{mode_value}', expected sequential|random|loop-playlist|loop-song"
                    )
                })?;
            }
            value if value.starts_with('-') => bail!("unknown flag '{value}'\n{}", usage(&bin)),
            value => {
                if playlist_path.is_some() {
                    bail!("multiple playlist paths provided\n{}", usage(&bin));
                }
                playlist_path = Some(PathBuf::from(value));
            }
        }
    }

    let playlist_path =
        playlist_path.ok_or_else(|| anyhow!("missing playlist path\n{}", usage(&bin)))?;

    Ok(CliArgs {
        playlist_path,
        start_index,
        mode,
    })
}

pub fn load_playlist_file(path: impl AsRef<Path>) -> Result<PlaylistFile> {
    let path = path.as_ref();
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read playlist file {}", path.display()))?;

    let mut entries = Vec::new();
    for (index, raw_line) in content.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let (query, resolved_id) = parse_playlist_line(line).with_context(|| {
            format!(
                "invalid playlist entry on line {} in {}",
                line_number,
                path.display()
            )
        })?;
        entries.push(PlaylistEntry {
            source_line: line_number,
            query,
            resolved_id,
        });
    }

    if entries.is_empty() {
        bail!(
            "playlist file {} contains no playable lines",
            path.display()
        );
    }

    Ok(PlaylistFile {
        path: path.to_path_buf(),
        name: playlist_name(path),
        entries,
    })
}

pub fn persist_resolved_id(path: &Path, source_line: usize, query: &str, id: YtId) -> Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read playlist file {}", path.display()))?;
    let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    let Some(line) = lines.get_mut(source_line.saturating_sub(1)) else {
        bail!(
            "playlist line {} disappeared while updating {}",
            source_line,
            path.display()
        );
    };

    *line = format!("{query} {ID_MARKER}{}", id.as_str());
    let mut updated = lines.join("\n");
    if content.ends_with('\n') {
        updated.push('\n');
    }
    fs::write(path, updated)
        .with_context(|| format!("failed to update playlist file {}", path.display()))
}

pub fn parse_playlist_line(line: &str) -> Result<(String, Option<YtId>)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        bail!("empty playlist line");
    }

    if let Ok(id) = parse_id_or_url(trimmed) {
        return Ok((trimmed.to_string(), Some(id)));
    }

    if let Some((query, id_text)) = trimmed.split_once(ID_MARKER) {
        let query = query.trim();
        if query.is_empty() {
            bail!("missing track query before '{ID_MARKER}'");
        }
        let id = parse_id_or_url(id_text.trim())
            .with_context(|| format!("invalid YouTube id after '{ID_MARKER}'"))?;
        return Ok((query.to_string(), Some(id)));
    }

    Ok((normalize_track_query(trimmed).to_string(), None))
}

pub fn parse_id_or_url(value: &str) -> Result<YtId> {
    let trimmed = value.trim();
    if let Ok(id) = YtId::parse(trimmed) {
        return Ok(id);
    }

    let url = Url::parse(trimmed).context("not a raw YouTube id or URL")?;
    if let Some(host) = url.host_str() {
        match host {
            "youtu.be" | "www.youtu.be" => {
                let id = url.path().trim_matches('/');
                return YtId::parse(id).context("invalid youtu.be video id");
            }
            "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" => {}
            _ => bail!("unsupported host '{host}'"),
        }
    }

    if let Some((_, value)) = url.query_pairs().find(|(key, _)| key == "v") {
        return YtId::parse(value.as_ref()).context("invalid watch video id");
    }

    let segments = url
        .path_segments()
        .ok_or_else(|| anyhow!("missing YouTube path segments"))?
        .collect::<Vec<_>>();
    for window in segments.windows(2) {
        match window {
            ["embed", id] | ["shorts", id] | ["live", id] => {
                return YtId::parse(id).context("invalid embedded/shorts/live video id");
            }
            _ => {}
        }
    }

    bail!("unsupported YouTube URL format")
}

pub fn normalize_track_query(value: &str) -> &str {
    let trimmed = value.trim();
    let Some((index, separator_len)) = leading_number_separator(trimmed) else {
        return trimmed;
    };
    let normalized = trimmed[index + separator_len..].trim_start();
    if normalized.is_empty() {
        trimmed
    } else {
        normalized
    }
}

fn parse_start(value: &str) -> Result<usize> {
    let one_based = value
        .parse::<usize>()
        .with_context(|| format!("invalid start index '{value}'"))?;
    Ok(one_based.saturating_sub(1))
}

fn usage(bin: &str) -> String {
    format!(
        "usage: {bin} <playlist.txt> [--start N] [--mode sequential|random|loop-playlist|loop-song]"
    )
}

fn playlist_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.replace(['_', '-'], " "))
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or_else(|| "playlist".to_string())
}

fn leading_number_separator(value: &str) -> Option<(usize, usize)> {
    let mut digit_count = 0usize;
    let mut end = 0usize;
    for (index, ch) in value.char_indices() {
        if ch.is_ascii_digit() {
            digit_count += 1;
            end = index + ch.len_utf8();
            if digit_count > 4 {
                return None;
            }
            continue;
        }
        break;
    }

    if digit_count == 0 || end >= value.len() {
        return None;
    }

    let rest = &value[end..];
    if rest.starts_with(". ") || rest.starts_with(") ") {
        Some((end, 2))
    } else if rest.starts_with(" - ") {
        Some((end, 3))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{PlaybackMode, normalize_track_query, parse_id_or_url, parse_playlist_line};

    #[test]
    fn normalizes_common_number_prefixes() {
        assert_eq!(
            normalize_track_query("1. Mees Salome - Ignorance Is Bliss"),
            "Mees Salome - Ignorance Is Bliss"
        );
        assert_eq!(
            normalize_track_query("02) Portishead Roads"),
            "Portishead Roads"
        );
        assert_eq!(
            normalize_track_query("003 - Massive Attack Teardrop"),
            "Massive Attack Teardrop"
        );
    }

    #[test]
    fn keeps_real_numbers() {
        assert_eq!(
            normalize_track_query("1979 Smashing Pumpkins"),
            "1979 Smashing Pumpkins"
        );
    }

    #[test]
    fn parses_marker_backed_id() {
        let (query, id) = parse_playlist_line("Massive Attack Teardrop # yt:dQw4w9WgXcQ").unwrap();
        assert_eq!(query, "Massive Attack Teardrop");
        assert_eq!(id.unwrap().as_str(), "dQw4w9WgXcQ");
    }

    #[test]
    fn parses_raw_id_lines() {
        let (query, id) = parse_playlist_line("dQw4w9WgXcQ").unwrap();
        assert_eq!(query, "dQw4w9WgXcQ");
        assert_eq!(id.unwrap().as_str(), "dQw4w9WgXcQ");
    }

    #[test]
    fn parses_watch_url() {
        let id = parse_id_or_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap();
        assert_eq!(id.as_str(), "dQw4w9WgXcQ");
    }

    #[test]
    fn mode_cycle_is_stable() {
        assert_eq!(PlaybackMode::Sequential.next(), PlaybackMode::Random);
        assert_eq!(PlaybackMode::LoopSong.next(), PlaybackMode::Sequential);
    }
}
