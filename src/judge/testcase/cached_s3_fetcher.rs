use std::{io::Read, num::NonZeroUsize, sync::Arc};

use async_trait::async_trait;
use flate2::read::MultiGzDecoder;
use prost::Message;
use tokio::sync::mpsc;

use super::{Testcase, TestcaseFetchError, TestcaseFetcher, Testcases};
use crate::judge::repository::s3_repository::CachedS3Repository;

pub struct CachedS3TestcaseFetcher {
    repository: Arc<CachedS3Repository<Testcase, String>>,
}

impl CachedS3TestcaseFetcher {
    pub fn new(client: aws_sdk_s3::Client, bucket: String, capacity: NonZeroUsize) -> Self {
        Self {
            repository: Arc::new(CachedS3Repository::new(
                client,
                bucket,
                capacity,
                decode_testcase_file,
            )),
        }
    }
}

#[async_trait]
impl TestcaseFetcher for CachedS3TestcaseFetcher {
    async fn get_subproblem_test_cases(
        &self,
        problem_id: i64,
        subproblem_id: i64,
    ) -> mpsc::UnboundedReceiver<Result<Testcase, TestcaseFetchError>> {
        self.repository
            .fetch(format!("{problem_id}/{subproblem_id}.pb.gz"))
            .await
    }
}

#[derive(Message)]
struct TestcaseFile {
    #[prost(message, repeated, tag = "1")]
    cases: Testcases,
}

fn decode_testcase_file(compressed: &[u8]) -> Result<Testcases, String> {
    let mut protobuf = Vec::new();
    MultiGzDecoder::new(compressed)
        .read_to_end(&mut protobuf)
        .map_err(|error| format!("invalid gzip: {error}"))?;
    let file = TestcaseFile::decode(bytes::Bytes::from(protobuf))
        .map_err(|error| format!("invalid protobuf: {error}"))?;
    Ok(file.cases)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};

    use super::*;

    #[test]
    fn decodes_gzipped_protobuf_pairs_in_order() {
        let protobuf = b"\x0a\x06\x0a\x01\xff\x12\x01\x00\x0a\x06\x0a\x01\x02\x12\x01\x03";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(protobuf).unwrap();
        let mut compressed = encoder.finish().unwrap();

        let cases = decode_testcase_file(&compressed).unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(&cases[0].input[..], &[0xff]);
        assert_eq!(&cases[0].answer[..], &[0x00]);
        assert_eq!(&cases[1].input[..], &[0x02]);
        assert_eq!(&cases[1].answer[..], &[0x03]);

        compressed.pop();
        assert!(decode_testcase_file(&compressed).is_err());
    }
}
