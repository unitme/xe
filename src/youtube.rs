use anyhow::{Context, Result, anyhow, bail};
use reqwest::{
    Client, Url,
    header::{ACCEPT, ACCEPT_LANGUAGE, COOKIE, ORIGIN, REFERER, USER_AGENT},
};
use serde_json::{Map, Value, json};
use liber::{Resolver, YtId, extract_player_path, find_n_bounds, solve_n_challenge};

const SEARCH_FILTER_SONG: &str = "EgWKAQIIAWoKEAkQBRAKEAMQBA==";
const YOUTUBE_MUSIC_API: &str = "https://music.youtube.com/youtubei/v1";
const INNERTUBE_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
const WEB_SAFARI_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.5 Safari/605.1.15,gzip(gfe)";
const USER_AGENT_WEB: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const TV_UA: &str = "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/25.lts.30.1034943-gold (unlike Gecko), Unknown_TV_Unknown_0/Unknown (Unknown, Unknown)";
const IOS_UA: &str =
    "com.google.ios.youtube/19.09.3 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)";
const ANDROID_VR_UA: &str = "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip";
const CONSENT_COOKIE: &str =
    "CONSENT=YES+; Path=/; Domain=.youtube.com; Secure; Expires=Fri, 01 Jan 2038 00:00:00 GMT";

#[derive(Clone)]
pub struct YoutubeService {
    http: Client,
}

#[derive(Debug, Clone)]
pub struct StreamUrl {
    pub url: String,
    pub user_agent: &'static str,
    pub referer: String,
    pub origin: &'static str,
}

impl StreamUrl {
    pub fn headers(&self) -> Vec<String> {
        vec![
            format!("Origin: {}", self.origin),
            format!("Referer: {}", self.referer),
        ]
    }
}

#[derive(Clone, Copy)]
struct SearchClient {
    client_name: &'static str,
    client_version: &'static str,
    client_id: &'static str,
    user_agent: &'static str,
}

#[derive(Clone, Copy)]
struct ClientProfile {
    client: InnertubeClient,
    user_agent: &'static str,
}

#[derive(Clone, Copy)]
enum InnertubeClient {
    Web,
    WebEmbedded,
    TvEmbedded,
    Ios,
    AndroidVr,
}

impl YoutubeService {
    pub fn new(http: Client) -> Self {
        Self { http }
    }

    pub async fn search_best_video_id(&self, query: &str) -> Result<YtId> {
        let response = self
            .http
            .post(format!("{YOUTUBE_MUSIC_API}/search"))
            .query(&[("prettyPrint", "false")])
            .header("Content-Type", "application/json")
            .header(USER_AGENT, WEB_REMIX.user_agent)
            .header("X-Goog-Api-Format-Version", "1")
            .header("X-YouTube-Client-Name", WEB_REMIX.client_id)
            .header("X-YouTube-Client-Version", WEB_REMIX.client_version)
            .header("X-Origin", "https://music.youtube.com")
            .header(REFERER, "https://music.youtube.com/")
            .json(&json!({
                "context": search_context(&WEB_REMIX),
                "query": query,
                "params": SEARCH_FILTER_SONG,
            }))
            .send()
            .await
            .with_context(|| format!("YouTube Music search failed for '{query}'"))?
            .error_for_status()
            .with_context(|| format!("YouTube Music rejected search for '{query}'"))?
            .json::<Value>()
            .await
            .with_context(|| format!("YouTube Music search JSON was invalid for '{query}'"))?;

        let matches = parse_search_matches(&response, 8);
        let first = matches
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no YouTube Music song result for '{query}'"))?;
        Ok(first)
    }

    pub async fn resolve_stream(&self, id: YtId) -> Result<StreamUrl> {
        let watch_html = fetch_watch_html(&self.http, id).await.ok();
        let visitor_data = watch_html.as_deref().and_then(extract_visitor_data);
        let player_js = if let Some(html) = &watch_html {
            fetch_player_js(&self.http, html.as_bytes()).await.ok()
        } else {
            None
        };

        if let Some(html) = &watch_html
            && let Ok(resolved) =
                Resolver::default().resolve_from_watch_html::<16384>(html.as_bytes())
        {
            let url = solve_n_in_url(
                resolved
                    .url()
                    .context("liber returned an invalid watch-page URL")?
                    .to_owned(),
                player_js.as_deref().map(str::as_bytes),
            );
            return Ok(StreamUrl {
                url,
                user_agent: WEB_SAFARI_UA,
                referer: watch_url(id),
                origin: "https://www.youtube.com",
            });
        }

        for profile in CLIENT_PROFILES {
            let response = match fetch_player_response(
                &self.http,
                *profile,
                id,
                visitor_data.as_deref(),
            )
            .await
            {
                Ok(response) => response,
                Err(_) => continue,
            };

            if let Ok(resolved) =
                Resolver::default().resolve_from_player_response::<16384>(response.as_bytes())
            {
                let url = solve_n_in_url(
                    resolved
                        .url()
                        .context("liber returned an invalid player-response URL")?
                        .to_owned(),
                    player_js.as_deref().map(str::as_bytes),
                );
                return Ok(StreamUrl {
                    url,
                    user_agent: profile.user_agent,
                    referer: watch_url(id),
                    origin: "https://www.youtube.com",
                });
            }

            if let Some(url) = extract_manifest_url(&response)? {
                return Ok(StreamUrl {
                    url: solve_n_in_url(url, player_js.as_deref().map(str::as_bytes)),
                    user_agent: profile.user_agent,
                    referer: watch_url(id),
                    origin: "https://www.youtube.com",
                });
            }
        }

        bail!("unable to resolve playable audio URL for {}", id.as_str())
    }
}

impl InnertubeClient {
    fn label(self) -> &'static str {
        match self {
            Self::Web => "WEB",
            Self::WebEmbedded => "WEB_EMBEDDED_PLAYER",
            Self::TvEmbedded => "TVHTML5_SIMPLY_EMBEDDED_PLAYER",
            Self::Ios => "IOS",
            Self::AndroidVr => "ANDROID_VR",
        }
    }

    fn client_id(self) -> &'static str {
        match self {
            Self::Web => "1",
            Self::WebEmbedded => "56",
            Self::TvEmbedded => "85",
            Self::Ios => "5",
            Self::AndroidVr => "28",
        }
    }

    fn client_version(self) -> &'static str {
        match self {
            Self::Web => "2.20260114.00.00",
            Self::WebEmbedded => "1.20260112.01.00",
            Self::TvEmbedded => "7.20260114.19.00",
            Self::Ios => "19.09.3",
            Self::AndroidVr => "1.65.10",
        }
    }

    fn context(self) -> Value {
        match self {
            Self::Web => json!({
                "clientName": "WEB",
                "clientVersion": self.client_version(),
                "hl": "en",
                "gl": "US",
            }),
            Self::WebEmbedded => json!({
                "clientName": "WEB_EMBEDDED_PLAYER",
                "clientVersion": self.client_version(),
                "hl": "en",
                "gl": "US",
            }),
            Self::TvEmbedded => json!({
                "clientName": "TVHTML5_SIMPLY_EMBEDDED_PLAYER",
                "clientVersion": self.client_version(),
                "hl": "en",
                "gl": "US",
            }),
            Self::Ios => json!({
                "clientName": "IOS",
                "clientVersion": self.client_version(),
                "deviceModel": "iPhone16,2",
                "osName": "iPhone",
                "osVersion": "18.3.2.22D82",
                "hl": "en",
                "gl": "US",
            }),
            Self::AndroidVr => json!({
                "clientName": "ANDROID_VR",
                "clientVersion": self.client_version(),
                "deviceMake": "Oculus",
                "deviceModel": "Quest 3",
                "osName": "Android",
                "osVersion": "12L",
                "androidSdkVersion": 32,
                "hl": "en",
                "gl": "US",
            }),
        }
    }
}

const WEB_REMIX: SearchClient = SearchClient {
    client_name: "WEB_REMIX",
    client_version: "1.20260114.01.00",
    client_id: "67",
    user_agent: USER_AGENT_WEB,
};

const CLIENT_PROFILES: &[ClientProfile] = &[
    ClientProfile {
        client: InnertubeClient::Web,
        user_agent: WEB_SAFARI_UA,
    },
    ClientProfile {
        client: InnertubeClient::WebEmbedded,
        user_agent: WEB_SAFARI_UA,
    },
    ClientProfile {
        client: InnertubeClient::TvEmbedded,
        user_agent: TV_UA,
    },
    ClientProfile {
        client: InnertubeClient::Ios,
        user_agent: IOS_UA,
    },
    ClientProfile {
        client: InnertubeClient::AndroidVr,
        user_agent: ANDROID_VR_UA,
    },
];

fn search_context(client: &SearchClient) -> Value {
    let mut inner = Map::new();
    inner.insert("clientName".to_string(), json!(client.client_name));
    inner.insert("clientVersion".to_string(), json!(client.client_version));
    inner.insert("gl".to_string(), json!("US"));
    inner.insert("hl".to_string(), json!("en"));

    json!({
        "client": Value::Object(inner),
        "user": {},
    })
}

fn parse_search_matches(value: &Value, limit: usize) -> Vec<YtId> {
    let mut matches = Vec::with_capacity(limit.min(8));
    let sections = value
        .pointer(
            "/contents/tabbedSearchResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents",
        )
        .and_then(Value::as_array);
    let Some(sections) = sections else {
        return matches;
    };

    for section in sections {
        let Some(contents) = section
            .get("musicShelfRenderer")
            .and_then(|shelf| shelf.get("contents"))
            .and_then(Value::as_array)
        else {
            continue;
        };

        for item in contents {
            let Some(video_id) = item
                .pointer("/musicResponsiveListItemRenderer/playlistItemData/videoId")
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Ok(id) = YtId::parse(video_id) else {
                continue;
            };
            if matches.iter().any(|seen| seen.as_str() == id.as_str()) {
                continue;
            }
            matches.push(id);
            if matches.len() >= limit {
                return matches;
            }
        }
    }

    matches
}

async fn fetch_watch_html(client: &Client, id: YtId) -> Result<String> {
    client
        .get(watch_url(id))
        .header(USER_AGENT, WEB_SAFARI_UA)
        .header(
            ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .header(COOKIE, CONSENT_COOKIE)
        .send()
        .await
        .context("failed to fetch watch page")?
        .error_for_status()
        .context("YouTube rejected watch page")?
        .text()
        .await
        .context("watch page response was not text")
}

async fn fetch_player_js(client: &Client, watch_html: &[u8]) -> Result<String> {
    const MAX_PATH: usize = 128;
    let path: liber::FixedStr<MAX_PATH> = extract_player_path(watch_html)
        .ok_or_else(|| anyhow!("player JS path not found in watch HTML"))?;
    let path_str = path.as_str().context("player path was not utf-8")?;
    let url = format!("https://www.youtube.com{path_str}");
    client
        .get(&url)
        .header(USER_AGENT, WEB_SAFARI_UA)
        .header(ACCEPT, "*/*")
        .send()
        .await
        .context("failed to fetch player JS")?
        .error_for_status()
        .context("YouTube rejected player JS request")?
        .text()
        .await
        .context("player JS response was not text")
}

fn solve_n_in_url(url: String, player_js: Option<&[u8]>) -> String {
    let Some(js) = player_js else {
        return url;
    };
    let Some((start, end)) = find_n_bounds(url.as_bytes()) else {
        return url;
    };
    let challenge = &url[start..end];
    let mut solved = liber::FixedStr::<64>::new();
    if solve_n_challenge(js, challenge, &mut solved).is_err() {
        return url;
    }
    let Ok(result) = solved.as_str() else {
        return url;
    };
    let mut new_url = String::with_capacity(url.len());
    new_url.push_str(&url[..start]);
    new_url.push_str(result);
    new_url.push_str(&url[end..]);
    new_url
}

fn extract_visitor_data(watch_html: &str) -> Option<String> {
    for key in [r#""VISITOR_DATA":""#, r#""visitorData":""#] {
        if let Some(start) = watch_html.find(key) {
            let after = start + key.len();
            let end = watch_html[after..].find('"')? + after;
            let value = &watch_html[after..end];
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

async fn fetch_player_response(
    client: &Client,
    profile: ClientProfile,
    id: YtId,
    visitor_data: Option<&str>,
) -> Result<String> {
    let url = format!("https://www.youtube.com/youtubei/v1/player?key={INNERTUBE_KEY}");
    let mut client_ctx = profile.client.context();
    if let Some(visitor_data) = visitor_data
        && let Some(obj) = client_ctx.as_object_mut()
    {
        obj.insert("visitorData".to_owned(), json!(visitor_data));
    }

    let context = if matches!(profile.client, InnertubeClient::TvEmbedded) {
        json!({
            "client": client_ctx,
            "thirdParty": { "embedUrl": "https://www.youtube.com/" },
        })
    } else {
        json!({ "client": client_ctx })
    };

    let body = json!({
        "context": context,
        "videoId": id.as_str(),
        "contentCheckOk": true,
        "racyCheckOk": true,
        "playbackContext": {
            "contentPlaybackContext": {
                "html5Preference": "HTML5_PREF_WANTS",
            }
        }
    });

    let mut request = client
        .post(url)
        .header(USER_AGENT, profile.user_agent)
        .header(ORIGIN, "https://www.youtube.com")
        .header("X-YouTube-Client-Name", profile.client.client_id())
        .header("X-YouTube-Client-Version", profile.client.client_version());
    if let Some(visitor_data) = visitor_data {
        request = request.header("X-Goog-Visitor-Id", visitor_data);
    }

    let text = request
        .json(&body)
        .send()
        .await
        .with_context(|| format!("player request failed for {}", profile.client.label()))?
        .error_for_status()
        .with_context(|| format!("player request rejected for {}", profile.client.label()))?
        .text()
        .await
        .with_context(|| {
            format!(
                "player response was not text for {}",
                profile.client.label()
            )
        })?;
    reject_unplayable(&text)?;
    Ok(text)
}

fn reject_unplayable(text: &str) -> Result<()> {
    let value: Value = serde_json::from_str(text).context("player response was not valid JSON")?;
    let status = value
        .pointer("/playabilityStatus/status")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN");
    if status == "OK" {
        return Ok(());
    }

    let reason = value
        .pointer("/playabilityStatus/reason")
        .and_then(Value::as_str)
        .unwrap_or("no reason");
    bail!("{status}: {reason}")
}

fn extract_manifest_url(player_response: &str) -> Result<Option<String>> {
    let value: Value =
        serde_json::from_str(player_response).context("player response was not valid JSON")?;
    let manifest = value
        .pointer("/streamingData/hlsManifestUrl")
        .and_then(Value::as_str);
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    Url::parse(manifest).context("invalid HLS manifest URL")?;
    Ok(Some(manifest.to_string()))
}

fn watch_url(id: YtId) -> String {
    format!("https://www.youtube.com/watch?v={}", id.as_str())
}
