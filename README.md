# KBO CLI

네이버 스포츠의 공개 JSON 응답을 사용해 KBO와 아시안게임 야구 실시간 전광판을 터미널에서 보는 Rust CLI입니다.

## 실행

```bash
cargo run
cargo run -- live
cargo run -- live --date 2026-07-07
cargo run -- asian-games
```

바이너리로 만들려면:

```bash
cargo build --release
./target/release/kbo-cli
./target/release/kbo-cli live
./target/release/kbo-cli asian-games
```

터미널 어디서나 `kbo-cli` 명령으로 쓰려면:

```bash
cargo install --path .
kbo-cli
kbo-cli live
kbo-cli asian-games
```

## 명령

```bash
kbo-cli
kbo-cli live [--date YYYY-MM-DD] [--interval 초] [--once]
kbo-cli asian-games [--date YYYY-MM-DD] [--interval 초] [--once]
kbo-cli ag [--date YYYY-MM-DD] [--interval 초] [--once]
```

아무 명령을 쓰지 않으면 `live` 모드로 실행합니다.
`live`는 KBO 전체 경기를 3 + 2 전광판 레이아웃으로 띄우고 주기적으로 갱신합니다.
`asian-games`는 아시안게임 야구 전체 경기를 같은 전광판으로 보여주며 `ag`로 줄여 쓸 수 있습니다.
터미널 안에서 `q`로 종료하고 `r`로 즉시 새로고침할 수 있습니다.

## 참고

네이버 스포츠 내부 API는 공식 공개 계약 API가 아니어서 경로/필드가 바뀔 수 있습니다.
현재 사용하는 주요 경로는 다음과 같습니다.

- `GET https://api-gw.sports.naver.com/schedule/games`
- `GET https://api-gw.sports.naver.com/schedule/games/{gameId}/game-polling`
