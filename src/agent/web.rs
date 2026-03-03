use crate::model::{IpcRequest, IpcResponse, WebConfig};
use crate::{agent, supervisor::Supervisor};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub fn spawn_server(
    config: WebConfig,
    supervisor: Arc<Mutex<Supervisor>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || run_server(config, supervisor))
}

fn run_server(config: WebConfig, supervisor: Arc<Mutex<Supervisor>>) {
    let addr = format!("{}:{}", config.host, config.port);
    let listener = match TcpListener::bind(&addr) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("web ui bind failed on {}: {}", addr, err);
            return;
        }
    };

    if let Err(err) = listener.set_nonblocking(true) {
        eprintln!("web ui set_nonblocking failed: {}", err);
        return;
    }

    while !agent::should_stop() {
        match listener.accept() {
            Ok((stream, _)) => {
                let cfg = config.clone();
                let sup = Arc::clone(&supervisor);
                thread::spawn(move || {
                    if let Err(err) = handle_connection(stream, &cfg, &sup) {
                        eprintln!("web ui request error: {}", err);
                    }
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(80));
            }
            Err(err) => {
                eprintln!("web ui accept error: {}", err);
                thread::sleep(Duration::from_millis(150));
            }
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    config: &WebConfig,
    supervisor: &Arc<Mutex<Supervisor>>,
) -> std::io::Result<()> {
    let mut buf = [0_u8; 8192];
    let read = stream.read(&mut buf)?;
    if read == 0 {
        return Ok(());
    }

    let req = String::from_utf8_lossy(&buf[..read]);
    let mut lines = req.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");

    let headers: Vec<String> = lines
        .take_while(|line| !line.trim().is_empty())
        .map(|s| s.to_string())
        .collect();

    let (path, query) = split_target(target);
    if path.starts_with("/api/") && !authorized(config, &headers, target) {
        return write_response(
            &mut stream,
            401,
            "application/json",
            "{\"error\":\"unauthorized\"}",
        );
    }

    match (method, path) {
        ("GET", "/") => write_response(&mut stream, 200, "text/html; charset=utf-8", WEB_HTML),
        ("GET", "/api/apps") => {
            let resp = handle_ipc_like(supervisor, IpcRequest::ListApps);
            write_json_response(&mut stream, resp)
        }
        ("GET", "/api/app") => {
            if let Some(name) = query_param(query, "name") {
                let resp = handle_ipc_like(supervisor, IpcRequest::GetApp { name });
                write_json_response(&mut stream, resp)
            } else {
                write_json_response(
                    &mut stream,
                    IpcResponse::err("missing query parameter 'name'"),
                )
            }
        }
        ("POST", "/api/start") => {
            if let Some(name) = query_param(query, "name") {
                let resp = handle_ipc_like(supervisor, IpcRequest::Start { name });
                write_json_response(&mut stream, resp)
            } else {
                write_json_response(
                    &mut stream,
                    IpcResponse::err("missing query parameter 'name'"),
                )
            }
        }
        ("POST", "/api/stop") => {
            if let Some(name) = query_param(query, "name") {
                let resp = handle_ipc_like(supervisor, IpcRequest::Stop { name });
                write_json_response(&mut stream, resp)
            } else {
                write_json_response(
                    &mut stream,
                    IpcResponse::err("missing query parameter 'name'"),
                )
            }
        }
        ("POST", "/api/restart") => {
            if let Some(name) = query_param(query, "name") {
                let resp = handle_ipc_like(supervisor, IpcRequest::Restart { name });
                write_json_response(&mut stream, resp)
            } else {
                write_json_response(
                    &mut stream,
                    IpcResponse::err("missing query parameter 'name'"),
                )
            }
        }
        ("GET", "/api/logs") => {
            if let Some(name) = query_param(query, "name") {
                let tail = query_param(query, "tail")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(200);
                let resp = handle_ipc_like(
                    supervisor,
                    IpcRequest::TailLogs {
                        name,
                        tail,
                        source: crate::model::LogSource::Both,
                    },
                );
                write_json_response(&mut stream, resp)
            } else {
                write_json_response(
                    &mut stream,
                    IpcResponse::err("missing query parameter 'name'"),
                )
            }
        }
        _ => write_response(
            &mut stream,
            404,
            "application/json",
            "{\"error\":\"not found\"}",
        ),
    }
}

fn handle_ipc_like(supervisor: &Arc<Mutex<Supervisor>>, req: IpcRequest) -> IpcResponse {
    let mut sup = match supervisor.lock() {
        Ok(guard) => guard,
        Err(_) => return IpcResponse::err("supervisor lock poisoned"),
    };

    match req {
        IpcRequest::Start { name } => match sup.start(&name) {
            Ok(started) => IpcResponse::ok(serde_json::json!({ "started": started })),
            Err(err) => IpcResponse::err(err.to_string()),
        },
        IpcRequest::Stop { name } => match sup.stop(&name) {
            Ok(stopped) => IpcResponse::ok(serde_json::json!({ "stopped": stopped })),
            Err(err) => IpcResponse::err(err.to_string()),
        },
        IpcRequest::Restart { name } => match sup.restart(&name) {
            Ok(restarted) => IpcResponse::ok(serde_json::json!({ "restarted": restarted })),
            Err(err) => IpcResponse::err(err.to_string()),
        },
        IpcRequest::ListApps => IpcResponse::ok(serde_json::json!({ "apps": sup.list_apps() })),
        IpcRequest::GetApp { name } => match sup.get_app(&name) {
            Ok(app) => IpcResponse::ok(serde_json::json!({ "app": app })),
            Err(err) => IpcResponse::err(err.to_string()),
        },
        IpcRequest::TailLogs { name, tail, source } => match sup.tail_logs(&name, source, tail) {
            Ok(lines) => IpcResponse::ok(serde_json::json!({ "lines": lines })),
            Err(err) => IpcResponse::err(err.to_string()),
        },
        _ => IpcResponse::err("unsupported request in web ui"),
    }
}

fn authorized(config: &WebConfig, headers: &[String], target: &str) -> bool {
    let Some(password) = config.password.as_ref() else {
        return true;
    };
    if password.is_empty() {
        return true;
    }

    let auth_bearer = format!("Bearer {}", password);
    for header in headers {
        let lower = header.to_ascii_lowercase();
        if lower.starts_with("authorization:") {
            if let Some((_, value)) = header.split_once(':') {
                if value.trim() == auth_bearer {
                    return true;
                }
            }
        }
        if lower.starts_with("x-pym2-password:") {
            if let Some((_, value)) = header.split_once(':') {
                if value.trim() == password {
                    return true;
                }
            }
        }
    }

    if let Some(pw) = query_param(split_target(target).1, "password") {
        if pw == *password {
            return true;
        }
    }

    false
}

fn split_target(target: &str) -> (&str, &str) {
    match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    }
}

fn query_param(query: &str, key: &str) -> Option<String> {
    for part in query.split('&') {
        let (k, v) = part.split_once('=')?;
        if k == key {
            return Some(url_decode(v));
        }
    }
    None
}

fn url_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let h = &input[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(h, 16) {
                    out.push(v as char);
                    i += 3;
                } else {
                    out.push('%');
                    i += 1;
                }
            }
            b => {
                out.push(b as char);
                i += 1;
            }
        }
    }
    out
}

fn write_json_response(stream: &mut TcpStream, resp: IpcResponse) -> std::io::Result<()> {
    let body = serde_json::to_string(&resp)
        .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"serialization error\"}".to_string());
    write_response(
        stream,
        if resp.ok { 200 } else { 400 },
        "application/json",
        &body,
    )
}

fn write_response(
    stream: &mut TcpStream,
    code: u16,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    let status = match code {
        200 => "200 OK",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        404 => "404 Not Found",
        _ => "500 Internal Server Error",
    };

    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        content_type,
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

const WEB_HTML: &str = r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>pym2 web</title>
  <style>
    body { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; margin: 20px; background: #f6f8fb; }
    h1 { margin-top: 0; }
    table { width: 100%; border-collapse: collapse; background: white; }
    th, td { border: 1px solid #dde3ea; padding: 8px; text-align: left; }
    button { margin-right: 6px; }
    pre { background: #111827; color: #f3f4f6; padding: 10px; min-height: 220px; overflow: auto; }
    .row { display: flex; gap: 8px; margin-bottom: 12px; }
    input { padding: 6px; }
  </style>
</head>
<body>
  <h1>pym2 web</h1>
  <div class="row">
    <input id="password" type="password" placeholder="password (optional)" />
    <button onclick="refreshApps()">refresh</button>
  </div>
  <table>
    <thead>
      <tr><th>name</th><th>status</th><th>pid</th><th>restarts</th><th>actions</th></tr>
    </thead>
    <tbody id="apps"></tbody>
  </table>
  <h3>logs</h3>
  <pre id="logs"></pre>
  <script>
    function authHeaders() {
      const pw = document.getElementById('password').value;
      if (!pw) return {};
      return { 'Authorization': 'Bearer ' + pw, 'X-Pym2-Password': pw };
    }

    async function api(path, opts={}) {
      const headers = Object.assign({}, authHeaders(), opts.headers || {});
      const res = await fetch(path, Object.assign({}, opts, { headers }));
      const data = await res.json();
      if (!data.ok) throw new Error(data.error || 'request failed');
      return data.data;
    }

    async function refreshApps() {
      try {
        const data = await api('/api/apps');
        const tbody = document.getElementById('apps');
        tbody.innerHTML = '';
        for (const app of data.apps) {
          const tr = document.createElement('tr');
          tr.innerHTML = `<td>${app.name}</td><td>${app.runtime.status}</td><td>${app.runtime.pid ?? '-'}</td><td>${app.runtime.restart_count}</td><td>
            <button onclick="act('start','${app.name}')">start</button>
            <button onclick="act('stop','${app.name}')">stop</button>
            <button onclick="act('restart','${app.name}')">restart</button>
            <button onclick="showLogs('${app.name}')">logs</button>
          </td>`;
          tbody.appendChild(tr);
        }
      } catch (e) {
        console.error(e);
      }
    }

    async function act(cmd, name) {
      try {
        await api('/api/' + cmd + '?name=' + encodeURIComponent(name), { method: 'POST' });
        await refreshApps();
      } catch (e) {
        alert(e.message);
      }
    }

    async function showLogs(name) {
      try {
        const data = await api('/api/logs?name=' + encodeURIComponent(name) + '&tail=200');
        document.getElementById('logs').textContent = data.lines.join('\n');
      } catch (e) {
        alert(e.message);
      }
    }

    setInterval(refreshApps, 2000);
    refreshApps();
  </script>
</body>
</html>"#;
