use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use flate2::{Compression, write::GzEncoder};
use prost::Message;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::Command,
    task::JoinHandle,
};

const ETAG: &str = "\"testcase-fixture-v1\"";
const OBJECT_PATH: &str = "/testcases/1/1.pb.gz";

pub struct MockS3 {
    endpoint: String,
    downloads: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl MockS3 {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let downloads = Arc::new(AtomicUsize::new(0));
        let fixture = testcase_fixture();
        let task = tokio::spawn({
            let downloads = Arc::clone(&downloads);
            async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    handle_request(stream, &fixture, &downloads).await.unwrap();
                }
            }
        });
        Self {
            endpoint,
            downloads,
            task,
        }
    }

    pub fn configure(&self, command: &mut Command) {
        command
            .env("RUSTFS_ENDPOINT", &self.endpoint)
            .env("RUSTFS_BUCKET", "testcases")
            .env("RUSTFS_REGION", "us-east-1")
            .env("RUSTFS_ACCESS_KEY", "test-access-key")
            .env("RUSTFS_SECRET_KEY", "test-secret-key");
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
    fixture: &[u8],
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

    let (status, length, body) = match (method, path) {
        (Some("HEAD"), Some(OBJECT_PATH)) => ("200 OK", fixture.len(), &[][..]),
        (Some("GET"), Some(OBJECT_PATH)) if matching_etag => {
            downloads.fetch_add(1, Ordering::Relaxed);
            ("200 OK", fixture.len(), fixture)
        }
        (Some("GET"), Some(OBJECT_PATH)) => ("412 Precondition Failed", 0, &[][..]),
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

#[derive(Message)]
struct TestcaseFile {
    #[prost(message, repeated, tag = "1")]
    cases: Vec<Testcase>,
}

#[derive(Message)]
struct Testcase {
    #[prost(bytes = "vec", tag = "1")]
    input: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    answer: Vec<u8>,
}

fn testcase_fixture() -> Vec<u8> {
    let file = TestcaseFile {
        cases: vec![
            Testcase {
                input: b"1 2\n".to_vec(),
                answer: b"3\n".to_vec(),
            },
            Testcase {
                input: b"100 200\n".to_vec(),
                answer: b"300\n".to_vec(),
            },
        ],
    };
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&file.encode_to_vec()).unwrap();
    encoder.finish().unwrap()
}
