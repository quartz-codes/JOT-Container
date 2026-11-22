// LcmpChecker: lines, ignores whitespaces
// It works the same as lcmp.cpp of Codeforces (https://github.com/MikeMirzayanov/testlib/blob/master/checkers/lcmp.cpp)

use std::iter::zip;

use super::Checker;
use async_trait::async_trait;

pub struct LcmpChecker;

impl LcmpChecker {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Checker for LcmpChecker {
    async fn check(
        &self,
        _input: &str,
        output: &str,
        answer: &str,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let output = output.split('\n');
        let answer = answer.split('\n');

        for (o, a) in zip(output, answer) {
            let o = o
                .split(' ')
                .map(|x| x.trim())
                .filter(|x| x.len() > 0)
                .collect::<Vec<_>>();
            let a = a
                .split(' ')
                .map(|x| x.trim())
                .filter(|x| x.len() > 0)
                .collect::<Vec<_>>();

            if o != a {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn check(output: &str, answer: &str) -> bool {
        let checker: Box<dyn Checker> = Box::new(LcmpChecker::new());
        let res = checker
            .check("", output, answer)
            .await
            .expect("Checker error");
        return res;
    }

    #[tokio::test]
    async fn test_1() {
        let output = "1 2";
        let answer = "1 2";

        assert_eq!(check(output, answer).await, true);
    }

    #[tokio::test]
    async fn test_2() {
        let output = "1     2\n";
        let answer = "1 2";

        assert_eq!(check(output, answer).await, true);
    }

    #[tokio::test]
    async fn test_3() {
        let output = "1     2 3\n 4 5";
        let answer = "1 2 3\n4 5";

        assert_eq!(check(output, answer).await, true);
    }

    #[tokio::test]
    async fn test_4() {
        let output = "1     2 3\n 4 5";
        let answer = "1 2\n3 4 5";

        assert_eq!(check(output, answer).await, false);
    }
    #[tokio::test]
    async fn test_5() {
        let output = "1     2 3\n\n 4 5";
        let answer = "1 2\n3 4 5";

        assert_eq!(check(output, answer).await, false);
    }
    #[tokio::test]
    async fn test_6() {
        let output = "1     2\n\n3   4     5         \n";
        let answer = "1 2\n\n3 4 5";

        assert_eq!(check(output, answer).await, true);
    }
}
