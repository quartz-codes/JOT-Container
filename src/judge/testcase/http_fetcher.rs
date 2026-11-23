use std::{io::Read, num::NonZeroUsize};

use async_trait::async_trait;
use bytes::Bytes;
use flate2::read::MultiGzDecoder;
use prost::Message;
use tokio::sync::mpsc;

use super::{TestcaseFetchError, Testcases, TestcasesFetcher};
use crate::judge::repository::{
    Repository,
    cache::{CachedRepository, Lru},
    http_repository::HttpRepository,
};

const TESTCASE_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(64).unwrap();

pub struct HttpTestcaseFetcher {
    repository:
        CachedRepository<HttpRepository<Testcases, String>, Lru<Testcases>, Testcases, String>,
}

impl HttpTestcaseFetcher {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            repository: CachedRepository::new(
                HttpRepository::new(client, decode_testcase_chunks),
                Lru::new(TESTCASE_CACHE_CAPACITY),
            ),
        }
    }
}

#[async_trait]
impl TestcasesFetcher for HttpTestcaseFetcher {
    async fn get_subproblem_testcases(
        &self,
        problem_id: i64,
        subproblem_id: i64,
        testcase_url: &str,
    ) -> Result<Testcases, TestcaseFetchError> {
        self.repository
            .fetch((problem_id, subproblem_id), testcase_url)
            .await
    }
}

#[derive(Message)]
struct TestcaseFile {
    #[prost(message, repeated, tag = "1")]
    cases: Testcases,
}

fn decode_testcase_chunks(
    mut receiver: mpsc::UnboundedReceiver<Bytes>,
) -> Result<Testcases, String> {
    // HttpRepository가 별도 블로킹 스레드에서 호출한다.
    // 송신 채널이 닫힐 때까지 청크를 받은 순서대로 이어 붙인다.
    let mut compressed = Vec::new();
    while let Some(chunk) = receiver.blocking_recv() {
        compressed.extend_from_slice(&chunk);
    }

    // gzip을 풀면 테스트케이스 목록을 담은 Protobuf 바이트가 나온다.
    let mut protobuf = Vec::new();
    MultiGzDecoder::new(compressed.as_slice())
        .read_to_end(&mut protobuf)
        .map_err(|error| format!("invalid gzip: {error}"))?;
    let file = TestcaseFile::decode(Bytes::from(protobuf))
        .map_err(|error| format!("invalid protobuf: {error}"))?;
    Ok(file.cases)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_chunks(compressed: &[u8]) -> Result<Testcases, String> {
        let (sender, receiver) = mpsc::unbounded_channel();
        for chunk in compressed.chunks(3) {
            sender.send(Bytes::copy_from_slice(chunk)).unwrap();
        }
        drop(sender);
        decode_testcase_chunks(receiver)
    }

    #[test]
    fn decodes_gzipped_protobuf_pairs_in_order() {
        let compressed = include_bytes!("../../../fixtures/aplusb_testcases.pb.gz");
        let cases = decode_chunks(compressed).unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(&cases[0].input[..], b"1 2");
        assert_eq!(&cases[0].answer[..], b"3");
        assert_eq!(&cases[1].input[..], b"100 200");
        assert_eq!(&cases[1].answer[..], b"300");

        assert!(decode_chunks(&compressed[..compressed.len() - 1]).is_err());
    }
}
