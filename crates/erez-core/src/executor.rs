use crate::plugins::Action;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use std::path::Path;
use std::{
    collections::HashMap, fmt, fs::File, io::BufReader, process::Command, sync::Arc, time::Duration,
};
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
    #[serde(default)]
    pub slots: HashMap<String, String>,
}

impl ActionOutcome {
    fn success(message: impl Into<String>) -> Self {
        Self {
            executed: true,
            message: message.into(),
            slots: HashMap::new(),
        }
    }

    fn with_slots(message: impl Into<String>, slots: HashMap<String, String>) -> Self {
        Self {
            executed: true,
            message: message.into(),
            slots,
        }
    }
}

pub trait TextSpeaker: Send + Sync {
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: f32,
        cache: bool,
    ) -> Result<ActionOutcome, ActionError>;
}

#[derive(Clone, Default)]
pub struct ActionExecutor {
    text_speaker: Option<Arc<dyn TextSpeaker>>,
}

impl fmt::Debug for ActionExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActionExecutor")
            .field("text_speaker", &self.text_speaker.is_some())
            .finish()
    }
}

impl ActionExecutor {
    pub fn with_text_speaker(text_speaker: Arc<dyn TextSpeaker>) -> Self {
        Self {
            text_speaker: Some(text_speaker),
        }
    }

    pub fn execute(&self, action: &Action) -> Result<ActionOutcome, ActionError> {
        match action {
            Action::EmitEvent { event, .. } => {
                Ok(ActionOutcome::success(format!("emitted event `{event}`")))
            }
            Action::Scenario { scenario_id, .. } => Ok(ActionOutcome {
                executed: false,
                message: format!("scenario `{scenario_id}` must be executed by ScenarioRunner"),
                slots: HashMap::new(),
            }),
            Action::OpenApp { app } => run_open_app(app),
            Action::SetVolume { level, delta } => run_set_volume(*level, *delta),
            Action::MediaControl { command, seconds } => run_media_control(command, *seconds),
            Action::PlaySound { file } | Action::SaySound { file } => run_play_sound(file),
            Action::SayText {
                text,
                voice,
                speed,
                cache,
            } => self
                .text_speaker
                .as_ref()
                .ok_or(ActionError::NotImplemented("say_text"))?
                .speak(text, voice.as_deref(), *speed, *cache),
            Action::Ask { .. } | Action::WaitForReply { .. } => Ok(ActionOutcome {
                executed: false,
                message: "dialog actions must be executed by ScenarioRunner".into(),
                slots: HashMap::new(),
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
                    slots: HashMap::new(),
                })
            }
            Action::Hotkey { keys } => run_hotkey(keys),
            Action::Url { url } => run_url(url),
            Action::HttpRequest {
                method,
                url,
                headers,
                body,
                response_slot,
                json_path,
                timeout_ms,
            } => run_http_request(
                method,
                url,
                headers,
                body,
                response_slot.as_deref(),
                json_path.as_deref(),
                *timeout_ms,
            ),
            Action::ConvertCurrency {
                amount,
                from,
                to,
                result_slot,
                api_url,
            } => run_currency_conversion(amount, from, to, result_slot, api_url),
            Action::Calculate {
                expression,
                result_slot,
            } => run_calculation(expression, result_slot),
            Action::Weather {
                location,
                fallback_location,
                result_slot,
            } => run_weather(location, fallback_location, result_slot),
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
        Action::SayText {
            text,
            voice,
            speed,
            cache,
        } => Action::SayText {
            text: interpolate_slots(text, slots, false),
            voice: voice
                .as_ref()
                .map(|voice| interpolate_slots(voice, slots, false)),
            speed: *speed,
            cache: *cache,
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
        Action::HttpRequest {
            method,
            url,
            headers,
            body,
            response_slot,
            json_path,
            timeout_ms,
        } => Action::HttpRequest {
            method: interpolate_slots(method, slots, false),
            url: interpolate_slots(url, slots, true),
            headers: headers
                .iter()
                .map(|(key, value)| {
                    (
                        interpolate_slots(key, slots, false),
                        interpolate_slots(value, slots, false),
                    )
                })
                .collect(),
            body: body.as_ref().map(|value| interpolate_json(value, slots)),
            response_slot: response_slot.clone(),
            json_path: json_path
                .as_ref()
                .map(|path| interpolate_slots(path, slots, false)),
            timeout_ms: *timeout_ms,
        },
        Action::ConvertCurrency {
            amount,
            from,
            to,
            result_slot,
            api_url,
        } => Action::ConvertCurrency {
            amount: interpolate_slots(amount, slots, false),
            from: interpolate_slots(from, slots, false),
            to: interpolate_slots(to, slots, false),
            result_slot: result_slot.clone(),
            api_url: interpolate_slots(api_url, slots, false),
        },
        Action::Calculate {
            expression,
            result_slot,
        } => Action::Calculate {
            expression: interpolate_slots(expression, slots, false),
            result_slot: result_slot.clone(),
        },
        Action::Weather {
            location,
            fallback_location,
            result_slot,
        } => Action::Weather {
            location: interpolate_slots(location, slots, false),
            fallback_location: interpolate_slots(fallback_location, slots, false),
            result_slot: result_slot.clone(),
        },
        Action::EmitEvent { event, payload } => Action::EmitEvent {
            event: interpolate_slots(event, slots, false),
            payload: interpolate_json(payload, slots),
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
        Action::MediaControl { command, seconds } => Action::MediaControl {
            command: interpolate_slots(command, slots, false),
            seconds: *seconds,
        },
        Action::Ask {
            sound,
            text,
            reply_slot,
        } => Action::Ask {
            sound: sound
                .as_ref()
                .map(|sound| interpolate_slots(sound, slots, false)),
            text: text
                .as_ref()
                .map(|text| interpolate_slots(text, slots, false)),
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

fn interpolate_json(
    value: &serde_json::Value,
    slots: &HashMap<String, String>,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(value) => {
            serde_json::Value::String(interpolate_slots(value, slots, false))
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| interpolate_json(value, slots))
                .collect(),
        ),
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), interpolate_json(value, slots)))
                .collect(),
        ),
        value => value.clone(),
    }
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
        slots: HashMap::new(),
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
            slots: HashMap::new(),
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
            slots: HashMap::new(),
        });
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let status = linux_set_volume(level, delta)?.status()?;
        return Ok(ActionOutcome {
            executed: status.success(),
            message: format!("set_volume exited with {status}"),
            slots: HashMap::new(),
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
            slots: HashMap::new(),
        });
    }
    let status = platform_open_app(app)?.status()?;
    Ok(ActionOutcome {
        executed: status.success(),
        message: format!("open_app exited with {status}"),
        slots: HashMap::new(),
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
        slots: HashMap::new(),
    })
}

fn run_http_request(
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: &Option<serde_json::Value>,
    response_slot: Option<&str>,
    json_path: Option<&str>,
    timeout_ms: u64,
) -> Result<ActionOutcome, ActionError> {
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|err| ActionError::Invalid(format!("invalid http method: {err}")))?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms.clamp(100, 120_000)))
        .build()?;
    let mut request = client.request(method, url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request.send()?;
    let status = response.status();
    let response_text = response.text()?;
    if !status.is_success() {
        return Ok(ActionOutcome {
            executed: false,
            message: format!(
                "http_request returned {status}: {}",
                truncate(&response_text, 240)
            ),
            slots: HashMap::new(),
        });
    }

    let mut slots = HashMap::new();
    if let Some(slot) = response_slot.filter(|slot| !slot.trim().is_empty()) {
        let value = if let Some(path) = json_path.filter(|path| !path.trim().is_empty()) {
            let json: serde_json::Value = serde_json::from_str(&response_text).map_err(|err| {
                ActionError::Invalid(format!("http response is not valid JSON: {err}"))
            })?;
            json_path_value(&json, path).ok_or_else(|| {
                ActionError::Invalid(format!("JSON path `{path}` was not found in response"))
            })?
        } else {
            response_text.clone()
        };
        slots.insert(slot.to_string(), value);
    }
    Ok(ActionOutcome::with_slots(
        format!("http_request returned {status}"),
        slots,
    ))
}

fn run_currency_conversion(
    amount: &str,
    from: &str,
    to: &str,
    result_slot: &str,
    api_url: &str,
) -> Result<ActionOutcome, ActionError> {
    let slots = crate::dynamic_actions::convert_currency(amount, from, to, result_slot, api_url)
        .map_err(ActionError::Invalid)?;
    Ok(ActionOutcome::with_slots(
        format!("converted {from} to {to}"),
        slots,
    ))
}

fn run_calculation(expression: &str, result_slot: &str) -> Result<ActionOutcome, ActionError> {
    let slots =
        crate::dynamic_actions::calculate(expression, result_slot).map_err(ActionError::Invalid)?;
    Ok(ActionOutcome::with_slots("calculation completed", slots))
}

fn run_weather(
    location: &str,
    fallback_location: &str,
    result_slot: &str,
) -> Result<ActionOutcome, ActionError> {
    let slots = crate::dynamic_actions::weather(location, fallback_location, result_slot)
        .map_err(ActionError::Invalid)?;
    Ok(ActionOutcome::with_slots("weather loaded", slots))
}

fn run_media_control(command: &str, seconds: Option<i32>) -> Result<ActionOutcome, ActionError> {
    let normalized = command.trim().to_lowercase();
    if normalized == "play_pause" || normalized == "pause" {
        #[cfg(target_os = "macos")]
        let status = macos_hotkey(&["space".into()])?.status()?;
        #[cfg(target_os = "windows")]
        let status = windows_hotkey(&["space".into()])?.status()?;
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let status = Command::new("playerctl").arg("play-pause").status()?;
        return Ok(ActionOutcome {
            executed: status.success(),
            message: format!("media play/pause exited with {status}"),
            slots: HashMap::new(),
        });
    }

    let direction = match normalized.as_str() {
        "seek_forward" | "forward" => 1,
        "seek_backward" | "backward" | "rewind" => -1,
        _ => {
            return Err(ActionError::Invalid(format!(
                "unknown media command `{command}`"
            )))
        }
    };
    let seconds = seconds.unwrap_or(30).unsigned_abs().clamp(1, 3600);

    #[cfg(target_os = "macos")]
    let status = {
        let key_code = if direction > 0 { 124 } else { 123 };
        let repeats = seconds.div_ceil(5);
        Command::new("osascript")
            .arg("-e")
            .arg(format!(
                "tell application \"System Events\" to repeat {repeats} times\nkey code {key_code}\ndelay 0.03\nend repeat"
            ))
            .status()?
    };
    #[cfg(target_os = "windows")]
    let status = {
        let key = if direction > 0 { "{RIGHT}" } else { "{LEFT}" };
        let repeats = seconds.div_ceil(5);
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; 1..{repeats} | ForEach-Object {{ [System.Windows.Forms.SendKeys]::SendWait('{key}'); Start-Sleep -Milliseconds 30 }}"
        );
        Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .status()?
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let status = {
        let offset = if direction > 0 {
            format!("{seconds}+")
        } else {
            format!("{seconds}-")
        };
        Command::new("playerctl")
            .args(["position", &offset])
            .status()?
    };

    Ok(ActionOutcome {
        executed: status.success(),
        message: format!("media seek {direction} {seconds}s exited with {status}"),
        slots: HashMap::new(),
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
        slots: HashMap::new(),
    })
}

fn json_path_value(value: &serde_json::Value, path: &str) -> Option<String> {
    let mut current = value;
    for part in path.trim_start_matches("$.").split('.') {
        if part.is_empty() {
            continue;
        }
        current = match current {
            serde_json::Value::Object(map) => map.get(part)?,
            serde_json::Value::Array(items) => items.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(match current {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Null => "null".into(),
        value => value.to_string(),
    })
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn platform_open_app(app: &str) -> Result<Command, ActionError> {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        let app = normalize_macos_app_path(app);
        if looks_like_app_path(&app) {
            command.arg(app);
        } else {
            command.arg("-a").arg(normalize_macos_app_name(&app));
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
case "$app" in
  "Terminal"|"terminal"|"терминал")
    for terminal in x-terminal-emulator gnome-terminal konsole xfce4-terminal mate-terminal alacritty kitty foot; do
      if command -v "$terminal" >/dev/null 2>&1; then
        exec "$terminal"
      fi
    done
    ;;
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

#[cfg(target_os = "macos")]
fn normalize_macos_app_name(app: &str) -> String {
    let trimmed = app.trim();
    match trimmed.to_lowercase().as_str() {
        "терминал" => "Terminal".to_string(),
        _ => trimmed.to_string(),
    }
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
        "f1" => "{F1}",
        "f2" => "{F2}",
        "f3" => "{F3}",
        "f4" => "{F4}",
        "f5" => "{F5}",
        "f6" => "{F6}",
        "f7" => "{F7}",
        "f8" => "{F8}",
        "f9" => "{F9}",
        "f10" => "{F10}",
        "f11" => "{F11}",
        "f12" => "{F12}",
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

    #[derive(Debug)]
    struct RecordingSpeaker(std::sync::Mutex<Vec<String>>);

    impl TextSpeaker for RecordingSpeaker {
        fn speak(
            &self,
            text: &str,
            _voice: Option<&str>,
            _speed: f32,
            _cache: bool,
        ) -> Result<ActionOutcome, ActionError> {
            self.0.lock().unwrap().push(text.into());
            Ok(ActionOutcome {
                executed: true,
                message: "spoken".into(),
                slots: HashMap::new(),
            })
        }
    }

    #[test]
    fn shell_is_disabled_by_default() {
        let action = Action::Shell {
            command: "echo".into(),
            args: vec!["hello".into()],
            enabled: false,
        };
        assert!(matches!(
            ActionExecutor::default().execute(&action),
            Err(ActionError::ShellDisabled)
        ));
    }

    #[test]
    fn rejects_non_http_urls() {
        let action = Action::Url {
            url: "file:///tmp/nope".into(),
        };
        assert!(matches!(
            ActionExecutor::default().execute(&action),
            Err(ActionError::Invalid(_))
        ));
    }

    #[test]
    fn extracts_nested_json_values_for_http_response_slots() {
        let response = serde_json::json!({"data": {"items": [{"value": 42}]}});
        assert_eq!(
            json_path_value(&response, "data.items.0.value").as_deref(),
            Some("42")
        );
        assert!(json_path_value(&response, "data.missing").is_none());
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

    #[test]
    fn say_text_interpolates_slots_and_uses_speaker() {
        let speaker = Arc::new(RecordingSpeaker(std::sync::Mutex::new(Vec::new())));
        let executor = ActionExecutor::with_text_speaker(speaker.clone());
        let action = apply_slots_to_action(
            &Action::SayText {
                text: "Заряд {{percent}} процентов".into(),
                voice: Some("komp".into()),
                speed: 1.0,
                cache: true,
            },
            &HashMap::from([("percent".into(), "57".into())]),
        );
        assert!(executor.execute(&action).unwrap().executed);
        assert_eq!(speaker.0.lock().unwrap().as_slice(), ["Заряд 57 процентов"]);
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
