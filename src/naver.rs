use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{
    Client,
    header::{ACCEPT, REFERER},
};
use serde::Deserialize;
use serde::de::DeserializeOwned;

const API_BASE: &str = "https://api-gw.sports.naver.com";
const MOBILE_BASE: &str = "https://m.sports.naver.com";
const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) ",
    "AppleWebKit/537.36 (KHTML, like Gecko) ",
    "Chrome/126.0 Safari/537.36 kbo-cli/0.1"
);

const SCHEDULE_FIELDS: &str = "basic,schedule,baseball,manualRelayUrl";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiResponse<T> {
    code: i32,
    success: bool,
    result: T,
}

#[derive(Clone, Debug)]
pub struct NaverClient {
    http: Client,
}

impl NaverClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(12))
            .build()
            .context("HTTP 클라이언트를 만들 수 없습니다")?;

        Ok(Self { http })
    }

    pub async fn baseball_games(&self, date: &str, category_id: &str) -> Result<Vec<Game>> {
        let url = format!("{API_BASE}/schedule/games");
        let result: ScheduleResult = self
            .get(
                &url,
                &[
                    ("fields", SCHEDULE_FIELDS),
                    ("upperCategoryId", "kbaseball"),
                    ("categoryId", category_id),
                    ("fromDate", date),
                    ("toDate", date),
                    ("size", "500"),
                ],
                Some(&format!(
                    "{MOBILE_BASE}/kbaseball/schedule/index?date={date}&category={category_id}"
                )),
            )
            .await?;

        Ok(result.games)
    }

    pub async fn game_polling(&self, game_id: &str) -> Result<GamePollingResult> {
        let url = format!("{API_BASE}/schedule/games/{game_id}/game-polling");
        self.get(
            &url,
            &[],
            Some(&format!("{MOBILE_BASE}/game/{game_id}/relay")),
        )
        .await
    }

    async fn get<T>(&self, url: &str, query: &[(&str, &str)], referer: Option<&str>) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let mut request = self.http.get(url).header(ACCEPT, "application/json");

        if !query.is_empty() {
            request = request.query(query);
        }

        if let Some(referer) = referer {
            request = request.header(REFERER, referer);
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("네이버 스포츠 API 요청 실패: {url}"))?
            .error_for_status()
            .with_context(|| format!("네이버 스포츠 API HTTP 오류: {url}"))?;

        let body: ApiResponse<T> = response
            .json()
            .await
            .with_context(|| format!("네이버 스포츠 API 응답 파싱 실패: {url}"))?;

        if !body.success {
            bail!("네이버 스포츠 API 오류(code={}): {url}", body.code);
        }

        Ok(body.result)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleResult {
    #[serde(default)]
    pub games: Vec<Game>,
    #[allow(dead_code)]
    pub game_total_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GamePollingResult {
    pub game: Game,
    pub text_relay_data: Option<TextRelayData>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub game_id: String,
    pub game_date_time: Option<String>,
    pub home_team_name: String,
    pub away_team_name: String,
    pub home_team_score: Option<i32>,
    pub away_team_score: Option<i32>,
    pub status_code: Option<String>,
    pub status_info: Option<String>,
    pub cancel: Option<bool>,
    pub suspended: Option<bool>,
    pub home_starter_name: Option<String>,
    pub away_starter_name: Option<String>,
    pub home_current_pitcher_name: Option<String>,
    pub away_current_pitcher_name: Option<String>,
    pub current_inning: Option<String>,
    #[serde(default)]
    pub home_team_score_by_inning: Vec<String>,
    #[serde(default)]
    pub away_team_score_by_inning: Vec<String>,
}

impl Game {
    pub fn is_started(&self) -> bool {
        self.status_code.as_deref() == Some("STARTED")
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.status_code.as_deref(), Some("RESULT" | "CANCEL"))
            || self.cancel.unwrap_or(false)
    }

    pub fn status_label(&self) -> &str {
        if self.cancel.unwrap_or(false) {
            "취소"
        } else if self.suspended.unwrap_or(false) {
            "중단"
        } else {
            match self.status_code.as_deref() {
                Some("BEFORE") => self.status_info.as_deref().unwrap_or("경기전"),
                Some("STARTED") => "LIVE",
                Some("RESULT") => "종료",
                Some("CANCEL") => "취소",
                _ => self.status_info.as_deref().unwrap_or("상태없음"),
            }
        }
    }

    pub fn starts_at_hhmm(&self) -> String {
        self.game_date_time
            .as_deref()
            .and_then(|value| value.split_once('T').map(|(_, time)| time))
            .map(|time| time.chars().take(5).collect())
            .unwrap_or_else(|| "--:--".to_string())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextRelayData {
    pub inn: Option<u32>,
    pub home_or_away: Option<String>,
    pub inning_score: Option<InningScore>,
    pub home_entry: Option<Lineup>,
    pub away_entry: Option<Lineup>,
    pub home_lineup: Option<Lineup>,
    pub away_lineup: Option<Lineup>,
    pub current_game_state: Option<GameState>,
    #[serde(default)]
    pub text_relays: Vec<TextRelayGroup>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InningScore {
    #[serde(default)]
    pub home: HashMap<String, String>,
    #[serde(default)]
    pub away: HashMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Lineup {
    #[serde(default)]
    pub batter: Vec<Player>,
    #[serde(default)]
    pub pitcher: Vec<Player>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    pub name: String,
    pub pcode: String,
    pub season_hra: Option<f64>,
    pub today_hra: Option<f64>,
    pub season_era: Option<String>,
    pub today_era: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextRelayGroup {
    pub no: Option<u64>,
    #[serde(default)]
    pub text_options: Vec<TextOption>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextOption {
    pub seqno: Option<u64>,
    pub text: Option<String>,
    pub current_game_state: Option<GameState>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GameState {
    pub home_score: Option<String>,
    pub away_score: Option<String>,
    pub pitcher: Option<String>,
    pub batter: Option<String>,
    pub strike: Option<String>,
    pub ball: Option<String>,
    pub out: Option<String>,
    pub base1: Option<String>,
    pub base2: Option<String>,
    pub base3: Option<String>,
}
