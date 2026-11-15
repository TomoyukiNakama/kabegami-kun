//! 壁紙の自動更新。systemd のユーザータイマーで、このアプリの `--apply` を定期的に実行する。
//!
//! snap 版は systemd のタイマーを作れないので、ログイン時に自動で起動する裏方（`--background`、`background.rs`）が代わりに動く。
//!
//! ```text
//! ~/.config/systemd/user/kabegami-kun.service   --apply を 1 回実行する
//! ~/.config/systemd/user/kabegami-kun.timer     実行する時刻
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};

use crate::config::{AutoMode, AutoUpdate};

const UNIT: &str = "kabegami-kun";
/// 名前を変える前（bing-wallpaper-manager）のタイマー。
const LEGACY_UNIT: &str = "bing-wallpaper-manager";

/// snap（strict）として動いているか。snap の中からは systemd のユーザータイマーを作れないので、
/// ログイン時の自動起動（snapd の autostart）と `--background` で自動更新する。
pub fn in_snap() -> bool {
    std::env::var_os("SNAP").is_some()
}

/// snap の autostart で起動するデスクトップファイルの名前（snapcraft.yaml の `autostart:` と一致させる）。
const AUTOSTART_FILE: &str = "kabegami-kun-background.desktop";

fn autostart_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("autostart").join(AUTOSTART_FILE)
}

/// snap 版: 自動更新するならログイン時に裏方を起動するよう登録し、今すぐにも起動する。しないなら登録を消す
/// （動いている裏方は、設定を読み直したときに自分で終わる）。
fn apply_snap(auto: &AutoUpdate) -> Result<()> {
    let path = autostart_path();
    if !auto.enabled {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| tr!("error.create_dir", path = dir.display()))?;
    }
    let entry = "[Desktop Entry]\nType=Application\nName=Kabegami-kun\nExec=kabegami-kun.background\nNoDisplay=true\n";
    fs::write(&path, entry).with_context(|| tr!("error.write", path = path.display()))?;
    start_background();
    Ok(())
}

/// 裏方（`--background`）を起動する。すでに動いていれば、新しい方はすぐに終わる。
pub fn start_background() {
    let Ok(exe) = std::env::current_exe() else { return };
    let child = Command::new(exe)
        .arg("--background")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn();
    // 終わった子を残さないよう、別のスレッドで待つ
    if let Ok(mut child) = child {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

fn unit_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("systemd").join("user")
}

/// `--apply` に付ける引数（空白区切り）。
pub fn apply_args(auto: &AutoUpdate, market: &str) -> Option<String> {
    if !auto.enabled {
        return None;
    }
    Some(match auto.mode {
        AutoMode::Today => format!("--apply --if-new --mkt {market}"),
        AutoMode::Favorites => "--apply --random --favorites".to_string(),
        AutoMode::Random => "--apply --random".to_string(),
    })
}

/// サービスとタイマーの内容。自動更新しない設定なら None。
fn unit_texts(exe: &Path, auto: &AutoUpdate, market: &str) -> Option<(String, String)> {
    let args = apply_args(auto, market)?;
    // 空白などを含むパスでも 1 つの引数になるように引用符で囲む（% は systemd の指定子なので重ねる）
    let exe = exe.display().to_string().replace('%', "%%").replace('\\', "\\\\").replace('"', "\\\"");
    let service = format!(
        "[Unit]\nDescription=Kabegami-kun: update the wallpaper\n\n\
         [Service]\nType=oneshot\nExecStart=\"{exe}\" {args}\n"
    );
    // 今日の写真: ログイン直後と 1 時間ごとに確かめ、新しい写真が配信されていたときだけ壁紙を変える
    let schedule = match auto.mode {
        AutoMode::Today => "OnStartupSec=1min\nOnCalendar=hourly\n".to_string(),
        _ => format!("OnCalendar={}\n", auto.interval.on_calendar()),
    };
    let timer = format!(
        "[Unit]\nDescription=Kabegami-kun: update the wallpaper\n\n\
         [Timer]\n{schedule}Persistent=true\n\n\
         [Install]\nWantedBy=timers.target\n"
    );
    Some((service, timer))
}

fn systemctl(args: &[&str]) -> Result<()> {
    // 撮影やテストでは、実際の systemd には触れない
    if std::env::var_os("BWM_NO_SYSTEMD").is_some() {
        return Ok(());
    }
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context(tr!("error.systemctl_missing"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(tr!("error.systemctl_failed", command = args.join(" "), detail = detail));
    }
    Ok(())
}

/// 設定に合わせてタイマーを作り直す（自動更新しない設定なら止めて消す）。
pub fn apply(auto: &AutoUpdate, market: &str) -> Result<()> {
    if in_snap() {
        return apply_snap(auto);
    }
    let dir = unit_dir();
    let service = dir.join(format!("{UNIT}.service"));
    let timer = dir.join(format!("{UNIT}.timer"));
    let timer_unit = format!("{UNIT}.timer");
    let exe = std::env::current_exe().context(tr!("error.current_exe"))?;
    match unit_texts(&exe, auto, market) {
        Some((service_text, timer_text)) => {
            fs::create_dir_all(&dir).with_context(|| tr!("error.create_dir", path = dir.display()))?;
            fs::write(&service, service_text).with_context(|| tr!("error.write", path = service.display()))?;
            fs::write(&timer, timer_text).with_context(|| tr!("error.write", path = timer.display()))?;
            systemctl(&["daemon-reload"])?;
            systemctl(&["enable", &timer_unit])?;
            // 動いているタイマーにも新しい時刻を反映する
            systemctl(&["restart", &timer_unit])?;
        }
        None => {
            if timer.exists() {
                // 止められなくても（systemd が無いなど）、ファイルは消す
                let stopped = systemctl(&["disable", "--now", &timer_unit]);
                let _ = fs::remove_file(&timer);
                let _ = fs::remove_file(&service);
                systemctl(&["daemon-reload"])?;
                stopped?;
            }
        }
    }
    Ok(())
}

/// 以前の名前のタイマーが残っていたら止めて消し、今の名前で作り直す（二重に動かさないため）。
pub fn migrate_legacy(auto: &AutoUpdate, market: &str) -> Result<()> {
    if in_snap() {
        return Ok(());
    }
    let dir = unit_dir();
    let timer = dir.join(format!("{LEGACY_UNIT}.timer"));
    let service = dir.join(format!("{LEGACY_UNIT}.service"));
    if !timer.exists() && !service.exists() {
        return Ok(());
    }
    // 止められなくても（systemd が無いなど）、ファイルは消す
    let _ = systemctl(&["disable", "--now", &format!("{LEGACY_UNIT}.timer")]);
    let _ = fs::remove_file(&timer);
    let _ = fs::remove_file(&service);
    let _ = systemctl(&["daemon-reload"]);
    apply(auto, market)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Interval;

    #[test]
    fn today_checks_hourly_and_only_applies_new_photos() {
        let auto = AutoUpdate { enabled: true, mode: AutoMode::Today, market: None, interval: Interval::Daily };
        let (service, timer) = unit_texts(Path::new("/opt/my apps/bwm"), &auto, "en-GB").unwrap();
        assert!(service.contains("ExecStart=\"/opt/my apps/bwm\" --apply --if-new --mkt en-GB\n"));
        assert!(timer.contains("OnStartupSec=1min\nOnCalendar=hourly\nPersistent=true\n"));
    }

    #[test]
    fn favorites_rotate_at_the_interval() {
        let auto = AutoUpdate { enabled: true, mode: AutoMode::Favorites, market: None, interval: Interval::Hours3 };
        let (service, timer) = unit_texts(Path::new("/usr/bin/bwm"), &auto, "ja-JP").unwrap();
        assert!(service.contains("ExecStart=\"/usr/bin/bwm\" --apply --random --favorites\n"));
        assert!(timer.contains("[Timer]\nOnCalendar=0/3:00\nPersistent=true\n"));
        assert!(unit_texts(Path::new("/usr/bin/bwm"), &AutoUpdate::default(), "ja-JP").is_none());
    }
}
