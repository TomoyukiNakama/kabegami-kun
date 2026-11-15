//! snap 版の自動更新。ログイン時に snapd の autostart で `--background` として起動し、
//! ログインしている間、設定に合わせて壁紙を変える（systemd のユーザータイマーの代わり）。
//!
//! 設定は毎回読み直すので、自動更新を切ると次の確認で終わる。二重に動かないよう、ロックファイルで 1 つに絞る。
//! 何をしたかは `~/snap/kabegami-kun/current/.config/kabegami-kun/background.log` に残す。

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{AutoMode, AutoUpdate, Config, State};
use crate::schedule;

/// 設定を確かめる間隔。
const TICK: Duration = Duration::from_secs(60);
/// 失敗したとき（ネットワークがまだ無いなど）に、やり直すまでの時間。
const RETRY_SECS: u64 = 5 * 60;
/// 「今日の写真」で、新しい写真が配信されたかを確かめる間隔。
const TODAY_SECS: u64 = 60 * 60;

fn lock_path() -> PathBuf {
    dirs::runtime_dir().unwrap_or_else(std::env::temp_dir).join("kabegami-kun-background.lock")
}

/// ログが大きくなりすぎないよう、この大きさを超えたら後半だけ残す。
const LOG_LIMIT: u64 = 64 * 1024;

/// 時刻付きでログに 1 件書く（書けなくても止めない）。
fn log(message: &str) {
    let path = crate::config::background_log_path();
    if fs::metadata(&path).is_ok_and(|m| m.len() > LOG_LIMIT) {
        if let Ok(text) = fs::read_to_string(&path) {
            let keep = text.len() / 2;
            let start = text[keep..].find('\n').map_or(keep, |i| keep + i + 1);
            let _ = fs::write(&path, &text[start..]);
        }
    }
    // 地域の時刻にするため date を使う（日時のライブラリを足さない）
    let time = std::process::Command::new("date")
        .arg("+%F %T")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| now().to_string());
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        for line in message.lines() {
            let _ = writeln!(file, "{time}  {line}");
        }
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// 前回の確認から、決めた間隔がたっているか（時計が戻ったときも確かめ直す）。
pub fn due(auto: &AutoUpdate, last_run: Option<u64>, now: u64) -> bool {
    let period = match auto.mode {
        AutoMode::Today => TODAY_SECS,
        _ => auto.interval.seconds(),
    };
    match last_run {
        None => true,
        Some(last) => now < last || now - last >= period,
    }
}

pub fn run() {
    let Ok(lock) = File::create(lock_path()) else { return };
    // すでに裏方が動いていれば、何もせずに終わる
    if lock.try_lock().is_err() {
        return;
    }
    log("started");
    let mut retry_at = 0;
    loop {
        let config = Config::load();
        if !config.auto.enabled {
            log("stopped: automatic updates are off");
            return;
        }
        let now = now();
        if now >= retry_at && due(&config.auto, State::load().last_run, now) {
            let args: Vec<String> = schedule::apply_args(&config.auto, config.auto_market())
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            log(&format!("--{}", args.join(" ").trim_start_matches("--")));
            match crate::run_cli(&args) {
                Ok(message) => {
                    log(&message);
                    let _ = State::update(|state| state.last_run = Some(now));
                }
                Err(err) => {
                    log(&format!("{}: {err:#}", tr!("cli.error")));
                    retry_at = now + RETRY_SECS;
                }
            }
        }
        sleep(TICK);
    }
}

#[cfg(test)]
mod tests {
    use super::due;
    use crate::config::{AutoMode, AutoUpdate, Interval};

    #[test]
    fn runs_when_the_interval_has_passed() {
        let today = AutoUpdate { enabled: true, mode: AutoMode::Today, market: None, interval: Interval::Daily };
        assert!(due(&today, None, 1000));
        assert!(!due(&today, Some(1000), 1000 + 59 * 60));
        assert!(due(&today, Some(1000), 1000 + 60 * 60));

        let random = AutoUpdate { mode: AutoMode::Random, interval: Interval::Hours3, ..today };
        assert!(!due(&random, Some(1000), 1000 + 2 * 60 * 60));
        assert!(due(&random, Some(1000), 1000 + 3 * 60 * 60));
        // 時計が戻ったら確かめ直す
        assert!(due(&random, Some(1000), 500));
    }
}
