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
    process::Command,
    task::JoinHandle,
};

const ETAG: &str = "\"testcase-fixture-v1\"";
const OBJECT_PATH: &str = "/testcases/1/7.pb.gz";
const SECOND_OBJECT_PATH: &str = "/testcases/1/12.pb.gz";
const FIXTURE: &[u8] = include_bytes!("../../fixtures/aplusb_testcases.pb.gz");
// A gzip member containing the empty Protobuf message.
const EMPTY_FIXTURE: &[u8] =
    b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00";

pub struct MockS3 {
    endpoint: String,
    head_requests: Arc<AtomicUsize>,
    downloads: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl MockS3 {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let head_requests = Arc::new(AtomicUsize::new(0));
        let downloads = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn({
            let head_requests = Arc::clone(&head_requests);
            let downloads = Arc::clone(&downloads);
            async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    handle_request(stream, &head_requests, &downloads)
                        .await
                        .unwrap();
                }
            }
        });
        Self {
            endpoint,
            head_requests,
            downloads,
            task,
        }
    }

    pub fn configure(&self, command: &mut Command) {
        command
            .env("S3_ENDPOINT", &self.endpoint)
            .env("S3_BUCKET", "testcases")
            .env("S3_REGION", "us-east-1")
            .env("S3_ACCESS_KEY", "test-access-key")
            .env("S3_SECRET_KEY", "test-secret-key");
    }

    pub fn head_requests(&self) -> usize {
        self.head_requests.load(Ordering::Relaxed)
    }

    pub fn downloads(&self) -> usize {
        self.downloads.load(Ordering::Relaxed)
    }
}

impl Drop for MockS3 {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle_request(
    stream: TcpStream,
    head_requests: &AtomicUsize,
    downloads: &AtomicUsize,
) -> io::Result<()> {
    let mut stream = BufReader::new(stream);
    let mut request_line = String::new();
    if stream.read_line(&mut request_line).await? == 0 {
        return Ok(());
    }
    let mut request = request_line.split_whitespace();
    let method = request.next();
    let path = request
        .next()
        .map(|target| target.split_once('?').map_or(target, |(path, _)| path));

    let mut matching_etag = false;
    let mut line = String::new();
    loop {
        line.clear();
        if stream.read_line(&mut line).await? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("if-match")
        {
            matching_etag = value.trim() == ETAG;
        }
    }

    let fixture = match path {
        Some(OBJECT_PATH | SECOND_OBJECT_PATH) => Some(FIXTURE),
        Some("/testcases/1/13.pb.gz") => Some(EMPTY_FIXTURE),
        _ => None,
    };
    let (status, length, body) = match (method, fixture) {
        (Some("HEAD"), Some(fixture)) => {
            head_requests.fetch_add(1, Ordering::Relaxed);
            ("200 OK", fixture.len(), &[][..])
        }
        (Some("GET"), Some(fixture)) if matching_etag => {
            downloads.fetch_add(1, Ordering::Relaxed);
            ("200 OK", fixture.len(), fixture)
        }
        (Some("GET"), Some(_)) => ("412 Precondition Failed", 0, &[][..]),
        _ => ("404 Not Found", 0, &[][..]),
    };
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {length}\r\nETag: {ETAG}\r\nConnection: close\r\n\r\n"
    );
    let stream = stream.get_mut();
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}
