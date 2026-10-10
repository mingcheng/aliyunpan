//! Minimal HTTP/1.1 test server built on tokio TcpListener without additional dependencies.
#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use aliyunpan::{Client, Config, Credentials, MemoryStore};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

pub const DEVICE_ID: &str = "0123456789abcdef0123456789abcdef";
pub const USER_ID: &str = "user-1";
pub const DRIVE_ID: &str = "drive-1";

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// Construct an absolute URL for this test server using the Host header.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.header("host").unwrap_or_default())
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub delay: Duration,
}

impl Response {
    pub fn json(value: Value) -> Self {
        Self::with_status(200, value)
    }

    pub fn with_status(status: u16, value: Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: value.to_string().into_bytes(),
            delay: Duration::ZERO,
        }
    }

    pub fn error(status: u16, code: &str) -> Self {
        Self::with_status(status, json!({ "code": code, "message": format!("mock {code}") }))
    }

    pub fn bytes(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body,
            delay: Duration::ZERO,
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

pub struct MockServer {
    pub url: String,
    log: Arc<Mutex<Vec<Request>>>,
}

impl MockServer {
    pub async fn start<F>(handler: F) -> Self
    where
        F: Fn(&Request) -> Response + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handler = Arc::new(handler);
        let log = Arc::new(Mutex::new(Vec::new()));
        let task_log = log.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let handler = handler.clone();
                let log = task_log.clone();
                tokio::spawn(async move {
                    if let Some(req) = read_request(&mut sock).await {
                        log.lock().unwrap().push(req.clone());
                        write_response(&mut sock, handler(&req)).await;
                    }
                });
            }
        });
        Self { url, log }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.log.lock().unwrap().clone()
    }

    pub fn count(&self, path_prefix: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path.starts_with(path_prefix))
            .count()
    }

    pub fn find(&self, path_prefix: &str) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|r| r.path.starts_with(path_prefix))
            .collect()
    }
}

async fn read_request(sock: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = std::str::from_utf8(&buf[..header_end]).ok()?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

async fn write_response(sock: &mut TcpStream, resp: Response) {
    tokio::time::sleep(resp.delay).await;
    let mut head = format!(
        "HTTP/1.1 {} MOCK\r\ncontent-length: {}\r\nconnection: close\r\n",
        resp.status,
        resp.body.len()
    );
    for (k, v) in &resp.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = sock.write_all(head.as_bytes()).await;
    let _ = sock.write_all(&resp.body).await;
    let _ = sock.shutdown().await;
}

/// Simulated token refresh and device sessions; `check` validates subsequent tokens and signatures.
#[derive(Default)]
pub struct Auth {
    pub refreshes: AtomicUsize,
    pub sessions: AtomicUsize,
    signature: Mutex<String>,
}

impl Auth {
    pub fn handle(&self, req: &Request) -> Option<Response> {
        match req.path.as_str() {
            "/v2/account/token" => {
                let current = self.refreshes.load(Ordering::SeqCst);
                if req.json()["refresh_token"] != format!("rt-{current}") {
                    return Some(Response::error(400, "InvalidParameter.RefreshToken"));
                }
                let n = self.refreshes.fetch_add(1, Ordering::SeqCst) + 1;
                Some(Response::json(json!({
                    "access_token": format!("at-{n}"),
                    "refresh_token": format!("rt-{n}"),
                    "expires_in": 7200,
                    "token_type": "Bearer",
                    "user_id": USER_ID,
                    "default_drive_id": DRIVE_ID,
                })))
            }
            "/users/v1/users/device/create_session" => {
                if req.header("authorization") != Some(&self.token()) {
                    return Some(Response::error(401, "AccessTokenInvalid"));
                }
                self.sessions.fetch_add(1, Ordering::SeqCst);
                *self.signature.lock().unwrap() = req.header("x-signature").unwrap_or_default().to_owned();
                Some(Response::json(
                    json!({ "result": true, "success": true, "code": "", "message": "" }),
                ))
            }
            _ => None,
        }
    }

    pub fn token(&self) -> String {
        format!("Bearer at-{}", self.refreshes.load(Ordering::SeqCst))
    }

    pub fn check(&self, req: &Request) -> Option<Response> {
        if req.header("authorization") != Some(&self.token()) {
            return Some(Response::error(401, "AccessTokenInvalid"));
        }
        if req.header("x-signature") != Some(self.signature.lock().unwrap().as_str()) {
            return Some(Response::error(400, "DeviceSessionSignatureInvalid"));
        }
        None
    }
}

pub fn config(url: &str) -> Config {
    let mut c = Config::with_base_url(url);
    c.retry_delay = Duration::from_millis(5);
    c.page_delay = Duration::ZERO;
    c.max_retries = 3;
    c
}

pub fn initial_credentials() -> Credentials {
    let mut c = Credentials::from_refresh_token("rt-0").unwrap();
    c.device_id = DEVICE_ID.into();
    c
}

pub async fn connect(server: &MockServer) -> (Client, Arc<MemoryStore>) {
    connect_with(config(&server.url), server).await
}

pub async fn connect_with(config: Config, _server: &MockServer) -> (Client, Arc<MemoryStore>) {
    let store = Arc::new(MemoryStore::new(initial_credentials()));
    let client = Client::connect_shared(config, store.clone()).await.expect("connect");
    (client, store)
}

pub fn file_json(file_id: &str, name: &str, kind: &str, parent: &str) -> Value {
    json!({
        "drive_id": DRIVE_ID,
        "file_id": file_id,
        "parent_file_id": parent,
        "name": name,
        "type": kind,
        "created_at": "2026-10-07T06:28:42.000Z",
        "updated_at": "2026-10-07T06:28:42.000Z",
    })
}

pub fn sign_in_json() -> Value {
    json!({
        "success": true, "code": null, "message": null,
        "totalCount": null, "nextToken": null, "maxResults": null, "arguments": null,
        "result": {
            "isSignIn": false, "year": "2026", "month": "\u{5341}\u{6708}", "day": "10", "signInDay": 5,
            "blessing": "private-blessing", "subtitle": "private-subtitle",
            "themeIcon": "https://example.invalid/private-icon",
            "themeAction": "smartdrive://webview?url=private-theme-action", "theme": "",
            "action": "smartdrive://webview?url=private-action",
            "rewards": [
                {
                    "id": null, "name": "private-reward", "rewardImage": "https://example.invalid/private-reward",
                    "rewardDesc": "private-description", "nameIcon": "", "type": "dailySignIn",
                    "actionText": null, "action": null, "status": "finished", "remind": "private-reminder",
                    "remindIcon": "", "expire": null, "position": 1, "idempotent": null
                },
                {
                    "id": null, "name": "private-task", "rewardImage": null, "rewardDesc": null,
                    "nameIcon": "", "type": "dailyTask", "actionText": null,
                    "action": "smartdrive://app/channel_backup", "status": "unfinished",
                    "remind": "private-task-reminder", "remindIcon": "", "expire": null,
                    "position": 2, "idempotent": null
                }
            ]
        }
    })
}

pub fn sha1_upper(data: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(data).iter().map(|b| format!("{b:02X}")).collect()
}

pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in input.chunks(3) {
        let n =
            (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
