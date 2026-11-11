//! 调试模式下的下游正文采集；内存预算与头尾截断在这里执行。

use sha2::{Digest, Sha256};

pub fn raw_body_text(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

pub fn capture_raw_body(bytes: &[u8]) -> Option<String> {
    if !crate::server::core::debug_traffic::enabled() {
        return None;
    }
    raw_body_text(bytes)
}

pub fn capture_raw_json<T: serde::Serialize>(value: &T) -> Option<String> {
    if !crate::server::core::debug_traffic::enabled() {
        return None;
    }
    serde_json::to_string(value).ok()
}

pub(super) struct RawCapture {
    enabled: bool,
    limit: usize,
    body: Vec<u8>,
    tail: Vec<u8>,
    overflow: bool,
    total: usize,
    digest: Sha256,
}

impl RawCapture {
    pub(super) fn new(enabled: bool, limit: usize) -> Self {
        Self {
            enabled,
            limit,
            body: Vec::new(),
            tail: Vec::new(),
            overflow: false,
            total: 0,
            digest: Sha256::new(),
        }
    }

    pub(super) fn push(&mut self, chunk: &[u8]) {
        if !self.enabled {
            return;
        }
        self.total = self.total.saturating_add(chunk.len());
        self.digest.update(chunk);
        let tail_limit = self.limit / 4;
        if self.overflow {
            update_tail(&mut self.tail, chunk, tail_limit);
            return;
        }
        if self.body.len().saturating_add(chunk.len()) <= self.limit {
            self.body.extend_from_slice(chunk);
            return;
        }
        self.overflow = true;
        let head_limit = self.limit.saturating_mul(3) / 4;
        if self.body.len() > head_limit {
            self.body.truncate(head_limit);
        }
        update_tail(&mut self.tail, chunk, tail_limit);
    }

    /// 定稿下游正文；与上游逐字节一致时返回 `None`，不保存第二份。
    pub(super) fn finish(
        &mut self,
        upstream: Option<&crate::server::core::debug_traffic::TrafficCapture>,
    ) -> Option<String> {
        if !self.enabled || self.total == 0 {
            return None;
        }
        let digest: [u8; 32] = self.digest.clone().finalize().into();
        if upstream.is_some_and(|capture| {
            capture.response_body_matches_sha256(self.total, &digest)
        }) {
            self.clear();
            return None;
        }
        let mut bytes = std::mem::take(&mut self.body);
        if self.overflow {
            let marker = format!(
                "\n[debug body truncated: original={} bytes]\n",
                self.total
            );
            bytes.extend_from_slice(marker.as_bytes());
            bytes.extend_from_slice(&self.tail);
        }
        self.tail.clear();
        // UTF-8 字符可能跨分片，收尾后才解码。
        Some(match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
        })
    }

    fn clear(&mut self) {
        self.body.clear();
        self.tail.clear();
        self.overflow = false;
        self.total = 0;
        self.digest = Sha256::new();
    }
}

fn update_tail(tail: &mut Vec<u8>, chunk: &[u8], limit: usize) {
    if limit == 0 {
        tail.clear();
        return;
    }
    if tail.len().saturating_add(chunk.len()) <= limit {
        tail.extend_from_slice(chunk);
        return;
    }
    if chunk.len() >= limit {
        tail.clear();
        tail.extend_from_slice(&chunk[chunk.len() - limit..]);
        return;
    }
    let keep = limit - chunk.len();
    if tail.len() > keep {
        let drop = tail.len() - keep;
        tail.drain(..drop);
    }
    tail.extend_from_slice(chunk);
}

#[cfg(test)]
mod tests {
    use super::{update_tail, RawCapture};

    #[test]
    fn repeated_upstream_body_is_not_duplicated() {
        let upstream = crate::server::core::debug_traffic::TrafficCapture::begin("same-body");
        upstream.reset_request("https://example.test", "test", &[], "{}");
        upstream.attach_response(200, &Default::default());
        upstream.push(b"same bytes");
        upstream.finish();
        let mut capture = RawCapture::new(true, 1024);
        capture.push(b"same bytes");
        assert!(capture.finish(Some(&upstream)).is_none());
    }

    #[test]
    fn oversized_downstream_keeps_bounded_head_and_tail() {
        let mut capture = RawCapture::new(true, 128);
        capture.push(&vec![b'a'; 128]);
        capture.push(&vec![b'b'; 32]);
        let body = capture.finish(None).unwrap_or_default();
        assert!(body.starts_with("aaa"));
        assert!(body.ends_with("bbbb"));
        assert!(body.contains("original=160 bytes"));
    }

    #[test]
    fn tail_window_keeps_latest_bytes() {
        let mut tail = vec![b'a'; 8];
        update_tail(&mut tail, b"12345", 8);
        assert_eq!(tail, b"aaa12345");
    }
}
