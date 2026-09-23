use std::time::Duration;

use anyhow::{Result, bail};
use chrono::{FixedOffset, NaiveDate, Utc};
use clap::{Args, Parser, Subcommand};

mod live;
mod naver;

use live::run_live;
use naver::NaverClient;

#[derive(Debug, Parser)]
#[command(
    name = "kbo-cli",
    version,
    about = "네이버 스포츠 KBO 실시간 전광판을 터미널에서 봅니다."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// 조회 날짜(YYYY-MM-DD). 생략하면 한국시간 오늘입니다.
    #[arg(short, long)]
    date: Option<NaiveDate>,

    /// 폴링 주기(초).
    #[arg(short, long, default_value_t = 5)]
    interval: u64,

    /// 한 번만 조회하고 종료합니다.
    #[arg(long)]
    once: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 오늘 KBO 전체 경기를 실시간 전광판으로 띄웁니다.
    Live(LiveArgs),
}

#[derive(Clone, Debug, Args)]
struct LiveArgs {
    /// 조회 날짜(YYYY-MM-DD). 생략하면 한국시간 오늘입니다.
    #[arg(short, long)]
    date: Option<NaiveDate>,

    /// 폴링 주기(초).
    #[arg(short, long, default_value_t = 5)]
    interval: u64,

    /// 한 번만 조회하고 종료합니다.
    #[arg(long)]
    once: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = NaverClient::new()?;

    let LiveArgs {
        date,
        interval,
        once,
    } = match cli.command {
        Some(Command::Live(args)) => args,
        None => LiveArgs {
            date: cli.date,
            interval: cli.interval,
            once: cli.once,
        },
    };

    if interval == 0 {
        bail!("--interval은 1초 이상이어야 합니다");
    }

    let date = date.unwrap_or_else(today_kst);
    run_live(
        &client,
        &date.to_string(),
        Duration::from_secs(interval),
        once,
    )
    .await
}

fn today_kst() -> NaiveDate {
    let kst = FixedOffset::east_opt(9 * 60 * 60).expect("KST offset is valid");
    Utc::now().with_timezone(&kst).date_naive()
}
