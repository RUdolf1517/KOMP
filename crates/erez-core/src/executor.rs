use crate::plugins::Action;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use std::path::Path;
use std::{collections::HashMap, fs::File, io::BufReader, process::Command};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ActionError {
    #[error("shell action is disabled by manifest")]
    ShellDisabled,
    #[error("failed to execute action: {0}")]
    Io(#[from] std::io::Error),
    #[error("http action failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid action: {0}")]
    Invalid(String),
    #[error("action type is declared but not executable yet: {0}")]
    NotImplemented(&'static str),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionOutcome {
    pub executed: bool,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct ActionExecutor;

impl ActionExecutor {
    pub fn execute(&self, action: &Action) -> Result<ActionOutcome, ActionError> {
        match action {
            Action::EmitEvent { event, .. } => Ok(ActionOutcome {
                executed: true,
                message: format!("emitted event `{event}`"),
            }),
            Action::Scenario { scenario_id, .. } => Ok(ActionOutcome {
                executed: false,
                message: format!("scenario `{scenario_id}` must be executed by ScenarioRunner"),
            }),
            Action::OpenApp { app } => run_open_app(app),
            Action::SetVolume { level, delta } => run_set_volume(*level, *delta),
            Action::PlaySound { file } | Action::SaySound { file } => run_play_sound(file),
            Action::Ask { .. } | Action::WaitForReply { .. } => Ok(ActionOutcome {
                executed: false,
                message: "dialog actions must be executed by ScenarioRunner".into(),
            }),
            Action::Shell {
                command,
                args,
                enabled,
            } => {
                if !enabled {
                    return Err(ActionError::ShellDisabled);
                }
                let status = Command::new(command).args(args).status()?;
                Ok(ActionOutcome {
                    executed: status.success(),
                    message: format!("shell exited with {status}"),
                })
            }
            Action::Hotkey { keys } => run_hotkey(keys),
            Action::Url { url } => run_url(url),
            Action::HttpRequest { method, url, body } => run_http_request(method, url, body),
        }
    }
}

pub fn apply_slots_to_action(action: &Action, slots: &HashMap<String, String>) -> Action {
    match action {
        Action::OpenApp { app } => Action::OpenApp {
            app: interpolate_slots(app, slots, false),
        },
        Action::PlaySound { file } => Action::PlaySound {
            file: interpolate_slots(file, slots, false),
        },
        Action::SaySound { file } => Action::SaySound {
            file: interpolate_slots(file, slots, false),
        },
        Action::Shell {
            command,
            args,
            enabled,
        } => Action::Shell {
            command: interpolate_slots(command, slots, false),
            args: args
                .iter()
                .map(|arg| interpolate_slots(arg, slots, false))
                .collect(),
            enabled: *enabled,
        },
        Action::Hotkey { keys } => Action::Hotkey {
            keys: keys
                .iter()
                .map(|key| interpolate_slots(key, slots, false))
                .collect(),
        },
        Action::Url { url } => Action::Url {
            url: interpolate_slots(url, slots, true),
        },
        Action::HttpRequest { method, url, body } => Action::HttpRequest {
            method: interpolate_slots(method, slots, false),
            url: interpolate_slots(url, slots, true),
            body: body.clone(),
        },
        Action::EmitEvent { event, payload } => Action::EmitEvent {
            event: interpolate_slots(event, slots, false),
            payload: payload.clone(),
        },
        Action::Scenario {
            plugin_id,
            scenario_id,
        } => Action::Scenario {
            plugin_id: plugin_id.clone(),
            scenario_id: scenario_id.clone(),
        },
        Action::SetVolume { level, delta } => Action::SetVolume {
            level: *level,
            delta: *delta,
        },
        Action::Ask { sound, reply_slot } => Action::Ask {
            sound: sound
                .as_ref()
                .map(|sound| interpolate_slots(sound, slots, false)),
            reply_slot: reply_slot.clone(),
        },
        Action::WaitForReply { reply_slot } => Action::WaitForReply {
            reply_slot: reply_slot.clone(),
        },
    }
}

fn interpolate_slots(input: &str, slots: &HashMap<String, String>, encode: bool) -> String {
    let mut output = input.to_string();
    for (key, value) in slots {
        let replacement = if encode {
            percent_encode(value)
        } else {
            value.clone()
        };
        output = output.replace(&format!("{{{{{key}}}}}"), &replacement);
    }
    output
}

fn percent_encode(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char)
            }
            b' ' => encoded.push('+'),
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

fn run_play_sound(file: &str) -> Result<ActionOutcome, ActionError> {
    crate::scenario::validate_sound_path(file)?;
    let stream = rodio::OutputStream::try_default()
        .map_err(|err| ActionError::Invalid(format!("audio output unavailable: {err}")))?;
    let (_stream, handle) = stream;
    let sink = rodio::Sink::try_new(&handle)
        .map_err(|err| ActionError::Invalid(format!("audio sink unavailable: {err}")))?;
    let file_handle = File::open(file)?;
    let source = rodio::Decoder::new(BufReader::new(file_handle))
        .map_err(|err| ActionError::Invalid(format!("failed to decode sound: {err}")))?;
    sink.append(source);
    sink.sleep_until_end();
    Ok(ActionOutcome {
        executed: true,
        message: format!("played sound `{file}`"),
    })
}

fn run_set_volume(level: Option<i32>, delta: Option<i32>) -> Result<ActionOutcome, ActionError> {
    if level.is_none() && delta.is_none() {
        return Err(ActionError::Invalid(
            "set_volume requires `level` or `delta`".into(),
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let script = if let Some(level) = level {
            format!("set volume output volume {}", level.clamp(0, 100))
        } else {
            let delta = delta.unwrap_or_default();
            format!(
                "set volume output volume ((output volume of (get volume settings)) + ({}))",
                delta
            )
        };
        let status = Command::new("osascript").arg("-e").arg(script).status()?;
        return Ok(ActionOutcome {
            executed: status.success(),
            message: format!("set_volume exited with {status}"),
        });
    }

    #[cfg(target_os = "windows")]
    {
        let step_count = delta.unwrap_or_default().unsigned_abs() / 2;
        let key = if delta.unwrap_or_default() < 0 {
            "[char]174"
        } else {
            "[char]175"
        };
        let script = if let Some(level) = level {
            format!(
                "$obj = New-Object -ComObject WScript.Shell; 50..1 | % {{$obj.SendKeys([char]174); Start-Sleep -Milliseconds 5}}; 1..{} | % {{$obj.SendKeys([char]175); Start-Sleep -Milliseconds 5}}",
                (level.clamp(0, 100) / 2).max(1)
            )
        } else {
            format!(
                "$obj = New-Object -ComObject WScript.Shell; 1..{} | % {{$obj.SendKeys({}); Start-Sleep -Milliseconds 5}}",
                step_count.max(1),
                key
            )
        };
        let status = Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .status()?;
        return Ok(ActionOutcome {
            executed: status.success(),
            message: format!("set_volume exited with {status}"),
        });
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let status = linux_set_volume(level, delta)?.status()?;
        return Ok(ActionOutcome {
            executed: status.success(),
            message: format!("set_volume exited with {status}"),
        });
    }
}

fn run_open_app(app: &str) -> Result<ActionOutcome, ActionError> {
    if app.trim().is_empty() {
        return Err(ActionError::Invalid("app cannot be empty".into()));
    }
    if app.trim().eq_ignore_ascii_case("browser") {
        let status = platform_open_url("https://www.google.com")?.status()?;
        return Ok(ActionOutcome {
            executed: status.success(),
            message: format!("open default browser exited with {status}"),
        });
    }
    let status = platform_open_app(app)?.status()?;
    Ok(ActionOutcome {
        executed: status.success(),
        message: format!("open_app exited with {status}"),
    })
}

fn run_url(url: &str) -> Result<ActionOutcome, ActionError> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(ActionError::Invalid(
            "url must start with http:// or https://".into(),
        ));
    }
    let status = platform_open_url(url)?.status()?;
    Ok(ActionOutcome {
        executed: status.success(),
        message: format!("url opener exited with {status}"),
    })
}

fn run_http_request(
    method: &str,
    url: &str,
    body: &Option<serde_json::Value>,
) -> Result<ActionOutcome, ActionError> {
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|err| ActionError::Invalid(format!("invalid http method: {err}")))?;
    let client = reqwest::blocking::Client::new();
    let mut request = client.request(method, url);
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request.send()?;
    Ok(ActionOutcome {
        executed: response.status().is_success(),
        message: format!("http_request returned {}", response.status()),
    })
}

fn run_hotkey(keys: &[String]) -> Result<ActionOutcome, ActionError> {
    if keys.is_empty() {
        return Err(ActionError::Invalid("hotkey keys cannot be empty".into()));
    }
    #[cfg(target_os = "macos")]
    let status = macos_hotkey(keys)?.status()?;
    #[cfg(target_os = "windows")]
    let status = windows_hotkey(keys)?.status()?;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let status = linux_hotkey(keys)?.status()?;

    Ok(ActionOutcome {
        executed: status.success(),
        message: format!("hotkey exited with {status}"),
    })
}

fn platform_open_app(app: &str) -> Result<Command, ActionError> {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        let app = normalize_macos_app_path(app);
        if looks_like_app_path(&app) {
            command.arg(app);
        } else {
            command.arg("-a").arg(app);
        }
        Ok(command)
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", app]);
        Ok(command)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(
                r#"app="$1"
case "$app" in
  "Google Chrome"|"Chrome"|"google chrome") app="google-chrome" ;;
  "Chromium"|"chromium") app="chromium" ;;
  "Firefox"|"firefox") app="firefox" ;;
esac
if command -v "$app" >/dev/null 2>&1; then
  exec "$app"
fi
if command -v gtk-launch >/dev/null 2>&1; then
  exec gtk-launch "$app"
fi
exec xdg-open "$app"
"#,
            )
            .arg("erez-open-app")
            .arg(app);
        Ok(command)
    }
}

#[cfg(target_os = "macos")]
fn looks_like_app_path(app: &str) -> bool {
    let trimmed = app.trim();
    Path::new(trimmed).is_absolute()
        || trimmed.contains(std::path::MAIN_SEPARATOR)
        || trimmed.ends_with(".app")
        || trimmed.ends_with(".exe")
}

#[cfg(target_os = "macos")]
fn normalize_macos_app_path(app: &str) -> String {
    let trimmed = app.trim();
    trimmed
        .strip_prefix("/Application/")
        .map(|rest| format!("/Applications/{rest}"))
        .unwrap_or_else(|| trimmed.to_string())
}

fn platform_open_url(url: &str) -> Result<Command, ActionError> {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg(url);
        Ok(command)
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", url]);
        Ok(command)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        Ok(command)
    }
}

#[cfg(target_os = "macos")]
fn macos_hotkey(keys: &[String]) -> Result<Command, ActionError> {
    let (key, modifiers) = split_key_and_modifiers(keys)?;
    if let Some(key_code) = macos_key_code(&key) {
        let modifier_clause = if modifiers.is_empty() {
            String::new()
        } else {
            format!(" using {{{}}}", modifiers.join(", "))
        };
        let mut command = Command::new("osascript");
        command.arg("-e").arg(format!(
            "tell application \"System Events\" to key code {key_code}{modifier_clause}"
        ));
        return Ok(command);
    }
    let modifier_clause = if modifiers.is_empty() {
        String::new()
    } else {
        format!(" using {{{}}}", modifiers.join(", "))
    };
    let mut command = Command::new("osascript");
    command.arg("-e").arg(format!(
        "tell application \"System Events\" to keystroke \"{}\"{}",
        key, modifier_clause
    ));
    Ok(command)
}

#[cfg(target_os = "macos")]
fn macos_key_code(key: &str) -> Option<u16> {
    Some(match key {
        "space" => 49,
        "left" | "arrow_left" => 123,
        "right" | "arrow_right" => 124,
        "down" | "arrow_down" => 125,
        "up" | "arrow_up" => 126,
        "page_down" | "pagedown" => 121,
        "page_up" | "pageup" => 116,
        "home" => 115,
        "end" => 119,
        _ => return None,
    })
}

#[cfg(target_os = "windows")]
fn windows_hotkey(keys: &[String]) -> Result<Command, ActionError> {
    let (key, modifiers) = split_key_and_modifiers(keys)?;
    let mut sequence = String::new();
    for modifier in modifiers {
        sequence.push_str(match modifier.as_str() {
            "command down" | "control down" => "^",
            "option down" => "%",
            "shift down" => "+",
            _ => "",
        });
    }
    sequence.push_str(windows_send_key(&key));
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait('{}')",
        sequence
    );
    let mut command = Command::new("powershell");
    command.args(["-NoProfile", "-Command", &script]);
    Ok(command)
}

#[cfg(target_os = "windows")]
fn windows_send_key(key: &str) -> &str {
    match key {
        "space" => " ",
        "left" | "arrow_left" => "{LEFT}",
        "right" | "arrow_right" => "{RIGHT}",
        "down" | "arrow_down" => "{DOWN}",
        "up" | "arrow_up" => "{UP}",
        "page_down" | "pagedown" => "{PGDN}",
        "page_up" | "pageup" => "{PGUP}",
        "home" => "{HOME}",
        "end" => "{END}",
        other => other,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn linux_set_volume(level: Option<i32>, delta: Option<i32>) -> Result<Command, ActionError> {
    if command_exists("wpctl") {
        let (program, args) = linux_volume_command(LinuxVolumeBackend::Wpctl, level, delta)?;
        let mut command = Command::new(program);
        command.args(args);
        return Ok(command);
    }

    if command_exists("pactl") {
        let (program, args) = linux_volume_command(LinuxVolumeBackend::Pactl, level, delta)?;
        let mut command = Command::new(program);
        command.args(args);
        return Ok(command);
    }

    Err(ActionError::NotImplemented(
        "set_volume requires wpctl or pactl",
    ))
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "windows"))))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxVolumeBackend {
    Wpctl,
    Pactl,
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "windows"))))]
fn linux_volume_command(
    backend: LinuxVolumeBackend,
    level: Option<i32>,
    delta: Option<i32>,
) -> Result<(&'static str, Vec<String>), ActionError> {
    if level.is_none() && delta.is_none() {
        return Err(ActionError::Invalid(
            "set_volume requires `level` or `delta`".into(),
        ));
    }

    match backend {
        LinuxVolumeBackend::Wpctl => {
            let mut args = vec!["set-volume".to_string(), "@DEFAULT_AUDIO_SINK@".to_string()];
            if let Some(level) = level {
                args.push(format!("{:.2}", level.clamp(0, 100) as f32 / 100.0));
            } else {
                let delta = delta.unwrap_or_default().clamp(-100, 100);
                let suffix = if delta < 0 { "%-" } else { "%+" };
                args.push(format!("{}{}", delta.abs(), suffix));
            }
            Ok(("wpctl", args))
        }
        LinuxVolumeBackend::Pactl => {
            let mut args = vec!["set-sink-volume".to_string(), "@DEFAULT_SINK@".to_string()];
            if let Some(level) = level {
                args.push(format!("{}%", level.clamp(0, 100)));
            } else {
                let delta = delta.unwrap_or_default().clamp(-100, 100);
                args.push(format!("{delta:+}%"));
            }
            Ok(("pactl", args))
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn linux_hotkey(keys: &[String]) -> Result<Command, ActionError> {
    if !command_exists("xdotool") {
        return Err(ActionError::NotImplemented("hotkey requires xdotool"));
    }
    let (key, modifiers) = split_key_and_modifiers(keys)?;
    let mut sequence = Vec::new();
    sequence.extend(modifiers);
    sequence.push(key);
    let mut command = Command::new("xdotool");
    command.arg("key").arg(sequence.join("+"));
    Ok(command)
}

fn split_key_and_modifiers(keys: &[String]) -> Result<(String, Vec<String>), ActionError> {
    let mut key = None;
    let mut modifiers = Vec::new();
    for raw in keys {
        match raw.to_lowercase().as_str() {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            "cmd" | "command" | "meta" | "win" => modifiers.push("command down".to_string()),
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            "cmd" | "command" | "meta" | "win" | "super" => modifiers.push("super".to_string()),
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            "ctrl" | "control" => modifiers.push("control down".to_string()),
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            "ctrl" | "control" => modifiers.push("ctrl".to_string()),
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            "alt" | "option" => modifiers.push("option down".to_string()),
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            "alt" | "option" => modifiers.push("alt".to_string()),
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            "shift" => modifiers.push("shift down".to_string()),
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            "shift" => modifiers.push("shift".to_string()),
            other => key = Some(other.to_string()),
        }
    }
    key.map(|key| (key, modifiers))
        .ok_or_else(|| ActionError::Invalid("hotkey must include a non-modifier key".into()))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn command_exists(command: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg("command -v \"$1\" >/dev/null 2>&1")
        .arg("erez-command-exists")
        .arg(command)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_is_disabled_by_default() {
        let action = Action::Shell {
            command: "echo".into(),
            args: vec!["hello".into()],
            enabled: false,
        };
        assert!(matches!(
            ActionExecutor.execute(&action),
            Err(ActionError::ShellDisabled)
        ));
    }

    #[test]
    fn rejects_non_http_urls() {
        let action = Action::Url {
            url: "file:///tmp/nope".into(),
        };
        assert!(matches!(
            ActionExecutor.execute(&action),
            Err(ActionError::Invalid(_))
        ));
    }

    #[test]
    fn applies_slots_to_url_actions_with_percent_encoding() {
        let action = Action::Url {
            url: "https://www.google.com/search?q={{query}}".into(),
        };
        let slots = HashMap::from([("query".to_string(), "что нибудь".to_string())]);
        let action = apply_slots_to_action(&action, &slots);

        assert_eq!(
            action,
            Action::Url {
                url: "https://www.google.com/search?q=%D1%87%D1%82%D0%BE+%D0%BD%D0%B8%D0%B1%D1%83%D0%B4%D1%8C".into()
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_app_path_accepts_common_singular_applications_typo() {
        assert_eq!(
            normalize_macos_app_path("/Application/Discord.app"),
            "/Applications/Discord.app"
        );
    }

    #[test]
    fn linux_wpctl_volume_command_supports_level_and_delta() {
        assert_eq!(
            linux_volume_command(LinuxVolumeBackend::Wpctl, Some(42), None).unwrap(),
            (
                "wpctl",
                vec![
                    "set-volume".to_string(),
                    "@DEFAULT_AUDIO_SINK@".to_string(),
                    "0.42".to_string()
                ]
            )
        );
        assert_eq!(
            linux_volume_command(LinuxVolumeBackend::Wpctl, None, Some(-15)).unwrap(),
            (
                "wpctl",
                vec![
                    "set-volume".to_string(),
                    "@DEFAULT_AUDIO_SINK@".to_string(),
                    "15%-".to_string()
                ]
            )
        );
    }

    #[test]
    fn linux_pactl_volume_command_supports_level_and_delta() {
        assert_eq!(
            linux_volume_command(LinuxVolumeBackend::Pactl, Some(120), None).unwrap(),
            (
                "pactl",
                vec![
                    "set-sink-volume".to_string(),
                    "@DEFAULT_SINK@".to_string(),
                    "100%".to_string()
                ]
            )
        );
        assert_eq!(
            linux_volume_command(LinuxVolumeBackend::Pactl, None, Some(-15)).unwrap(),
            (
                "pactl",
                vec![
                    "set-sink-volume".to_string(),
                    "@DEFAULT_SINK@".to_string(),
                    "-15%".to_string()
                ]
            )
        );
    }
}
