use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

const OBJECT_PATH: &str = "/testcases/1/7.pb.gz";
const SECOND_OBJECT_PATH: &str = "/testcases/1/12.pb.gz";
const FIXTURE: &[u8] = include_bytes!("../../fixtures/aplusb_testcases.pb.gz");
// A gzip member containing the empty Protobuf message.
const EMPTY_FIXTURE: &[u8] =
    b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00";

pub struct MockHttp {
    endpoint: String,
    downloads: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl MockHttp {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let downloads = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn({
            let downloads = Arc::clone(&downloads);
            async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    handle_request(stream, &downloads).await.unwrap();
                }
            }
        });
        Self {
            endpoint,
            downloads,
            task,
        }
    }

    pub fn url(&self, subproblem_id: i64) -> String {
        format!(
            "{}/testcases/1/{subproblem_id}.pb.gz?signature=test%2Bsignature",
            self.endpoint
        )
    }

    pub fn downloads(&self) -> usize {
        self.downloads.load(Ordering::Relaxed)
    }
}

impl Drop for MockHttp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle_request(stream: TcpStream, downloads: &AtomicUsize) -> io::Result<()> {
    let mut stream = BufReader::new(stream);
    let mut request_line = String::new();
    if stream.read_line(&mut request_line).await? == 0 {
        return Ok(());
    }
    let mut request = request_line.split_whitespace();
    let method = request.next();
    let target = request.next().unwrap();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = (query == "signature=test%2Bsignature").then_some(path);

    let mut conditional_request = false;
    let mut line = String::new();
    loop {
        line.clear();
        if stream.read_line(&mut line).await? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, _)) = line.split_once(':')
            && (name.eq_ignore_ascii_case("if-match") || name.eq_ignore_ascii_case("if-none-match"))
        {
            conditional_request = true;
        }
    }

    let fixture = match path {
        Some(OBJECT_PATH | SECOND_OBJECT_PATH) => Some(FIXTURE),
        Some("/testcases/1/13.pb.gz") => Some(EMPTY_FIXTURE),
        _ => None,
    };
    let (status, length, body) = match (method, fixture) {
        (Some("GET"), Some(fixture)) if !conditional_request => {
            downloads.fetch_add(1, Ordering::Relaxed);
            ("200 OK", fixture.len(), fixture)
        }
        _ => ("404 Not Found", 0, &[][..]),
    };
    let headers =
        format!("HTTP/1.1 {status}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
    let stream = stream.get_mut();
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}
