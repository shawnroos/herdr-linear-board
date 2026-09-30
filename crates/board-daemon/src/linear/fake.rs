use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Recorded {
    pub authorization: Option<String>,
    pub content_type: Option<String>,
    pub body: Value,
}

pub enum Reply {
    Json {
        status: u16,
        body: Value,
        headers: Vec<(String, String)>,
    },
    Stall(Duration),
}

impl Reply {
    pub fn ok(body: Value) -> Self {
        Reply::Json {
            status: 200,
            body,
            headers: Vec::new(),
        }
    }

    pub fn status(status: u16, body: Value) -> Self {
        Reply::Json {
            status,
            body,
            headers: Vec::new(),
        }
    }
}

type Handler = dyn Fn(&Recorded, usize) -> Reply + Send + Sync;

pub struct FakeLinear {
    port: u16,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl FakeLinear {
    pub fn start(handler: impl Fn(&Recorded, usize) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let counter = Arc::new(AtomicUsize::new(0));
        let recorded = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let handler = handler.clone();
                let recorded = recorded.clone();
                let counter = counter.clone();
                std::thread::spawn(move || serve(stream, &*handler, &recorded, &counter));
            }
        });
        FakeLinear { port, requests }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/graphql", self.port)
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().expect("requests lock").clone()
    }
}

fn serve(
    stream: TcpStream,
    handler: &Handler,
    recorded: &Mutex<Vec<Recorded>>,
    counter: &AtomicUsize,
) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut authorization = None;
    let mut content_type = None;
    let mut length = 0usize;
    let mut line = String::new();
    let mut first = true;
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let trimmed = line.trim_end();
        if first {
            first = false;
            continue;
        }
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            let value = value.trim().to_string();
            match name.trim().to_ascii_lowercase().as_str() {
                "authorization" => authorization = Some(value),
                "content-type" => content_type = Some(value),
                "content-length" => length = value.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    let mut body = vec![0u8; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let request = Recorded {
        authorization,
        content_type,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    };
    recorded
        .lock()
        .expect("requests lock")
        .push(request.clone());
    let index = counter.fetch_add(1, Ordering::SeqCst);
    let mut stream = stream;
    match handler(&request, index) {
        Reply::Stall(duration) => std::thread::sleep(duration),
        Reply::Json {
            status,
            body,
            headers,
        } => {
            let payload = body.to_string();
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                payload.len()
            );
            for (name, value) in headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(payload.as_bytes());
            let _ = stream.flush();
        }
    }
}
