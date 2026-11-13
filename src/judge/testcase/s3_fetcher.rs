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
    s3_repository::S3Repository,
};

const TESTCASE_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(64).unwrap();

pub struct S3TestcaseFetcher {
    repository:
        CachedRepository<S3Repository<Testcases, String>, Lru<Testcases>, Testcases, String>,
}

impl S3TestcaseFetcher {
    pub fn new(client: aws_sdk_s3::Client, bucket: String) -> Self {
        Self {
            repository: CachedRepository::new(
                S3Repository::new(client, bucket, decode_testcase_chunks),
                Lru::new(TESTCASE_CACHE_CAPACITY),
            ),
        }
    }
}

#[async_trait]
impl TestcasesFetcher for S3TestcaseFetcher {
    async fn get_subproblem_testcases(
        &self,
        problem_id: i64,
        subproblem_id: i64,
    ) -> Result<Testcases, TestcaseFetchError> {
        let object_key = format!("{problem_id}/{subproblem_id}.pb.gz");
        self.repository.fetch(&object_key).await
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
    // S3Repository가 별도 블로킹 스레드에서 호출한다.
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
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};

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
        let protobuf = b"\x0a\x06\x0a\x01\xff\x12\x01\x00\x0a\x06\x0a\x01\x02\x12\x01\x03";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(protobuf).unwrap();
        let mut compressed = encoder.finish().unwrap();

        let cases = decode_chunks(&compressed).unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(&cases[0].input[..], &[0xff]);
        assert_eq!(&cases[0].answer[..], &[0x00]);
        assert_eq!(&cases[1].input[..], &[0x02]);
        assert_eq!(&cases[1].answer[..], &[0x03]);

        compressed.pop();
        assert!(decode_chunks(&compressed).is_err());
    }
}
