use std::{
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::Command,
    time::Duration,
};

#[tauri::command]
async fn http_json(
    method: String,
    path: String,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:3737{path}");
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|err| err.to_string())?;
    let mut request = client.request(method, url);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.map_err(|err| err.to_string())?;
    let status = response.status();
    let payload = response
        .json::<serde_json::Value>()
        .await
        .map_err(|err| err.to_string())?;
    if status.is_success() {
        Ok(payload)
    } else {
        Err(payload
            .get("error")
            .and_then(|value| value.as_str())
            .unwrap_or("request failed")
            .to_string())
    }
}

fn main() {
    tauri::Builder::default()
        .setup(|_app| {
            ensure_daemon_started();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![http_json])
        .run(tauri::generate_context!())
        .expect("failed to run KOMP desktop app");
}

fn ensure_daemon_started() {
    if daemon_health_ok() {
        return;
    }

    let Some(root) = workspace_root() else {
        eprintln!("KOMP desktop: failed to find workspace root for daemon autostart");
        return;
    };

    let mut command = Command::new("cargo");
    command
        .args(["run", "-p", "erez-daemon"])
        .current_dir(&root);
    let prototype_config = root.join("komp.prototype.toml");
    if prototype_config.exists() {
        command.env("KOMP_CONFIG", prototype_config);
    }

    match command.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            std::thread::sleep(Duration::from_millis(350));
        }
        Err(err) => eprintln!("KOMP desktop: failed to autostart daemon: {err}"),
    }
}

fn daemon_health_ok() -> bool {
    let Ok(mut stream) = TcpStream::connect("127.0.0.1:3737") else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(350)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(350)));
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = String::new();
    stream.read_to_string(&mut response).is_ok()
        && response.starts_with("HTTP/1.1 200")
        && response.contains("\"status\":\"ok\"")
}

fn workspace_root() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent()?.parent()?.parent().map(PathBuf::from)
}
