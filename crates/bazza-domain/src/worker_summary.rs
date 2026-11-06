//! Bounded automatic summary result for a worker's already-pinned transcript.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, io};

pub const MAX_WORKER_SUMMARY_BODY_BYTES: u64 = 2 * 1024 * 1024;
const MAX_DOCUMENT_BYTES: usize = 262_144;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerGeneratedSummary {
    pub expected_summary_revision: String,
    pub template_id: String,
    pub expected_template_revision: String,
    pub expected_template_generation: String,
    pub model: String,
    pub markdown: String,
    pub structured_json: Value,
}

impl fmt::Debug for WorkerGeneratedSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkerGeneratedSummary")
            .field("markdown_bytes", &self.markdown.len())
            .finish_non_exhaustive()
    }
}

impl WorkerGeneratedSummary {
    pub fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        use serde::de::Error;
        if bytes.len() as u64 > MAX_WORKER_SUMMARY_BODY_BYTES {
            return Err(serde_json::Error::custom(
                "generated summary body exceeds byte budget",
            ));
        }
        if bytes
            .iter()
            .copied()
            .find(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
            != Some(b'{')
        {
            return Err(serde_json::Error::custom(
                "generated summary body must be an object",
            ));
        }
        let body: Self = serde_json::from_slice(bytes)?;
        if !body.valid() {
            return Err(serde_json::Error::custom(
                "invalid generated summary result",
            ));
        }
        Ok(body)
    }

    pub fn valid(&self) -> bool {
        valid_revision(&self.expected_summary_revision)
            && valid_revision(&self.expected_template_revision)
            && valid_revision(&self.expected_template_generation)
            && (1..=256).contains(&self.template_id.len())
            && !self.template_id.contains('\0')
            && (1..=256).contains(&self.model.len())
            && !self.model.contains('\0')
            && self.markdown.len() <= MAX_DOCUMENT_BYTES
            && !self.markdown.contains('\0')
            && serde_json::to_writer(ByteBudget(MAX_DOCUMENT_BYTES), &self.structured_json).is_ok()
    }
}

fn valid_revision(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
}

struct ByteBudget(usize);

impl io::Write for ByteBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("generated summary document exceeds byte budget"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body() -> WorkerGeneratedSummary {
        WorkerGeneratedSummary {
            expected_summary_revision: "summary-original".into(),
            template_id: "Повестка встречи".into(),
            expected_template_revision: "template-original".into(),
            expected_template_generation: "template-generation".into(),
            model: "fixture local model".into(),
            markdown: "  **Сводка**\n\t🦀  \n".into(),
            structured_json: json!({"nested": [null, 7, "\u{0000}"]}),
        }
    }

    #[test]
    fn exact_markdown_and_structured_values_survive_a_closed_object() {
        let body = body();
        let bytes = serde_json::to_vec(&body).unwrap();
        assert_eq!(WorkerGeneratedSummary::parse(&bytes).unwrap(), body);
        assert!(!format!("{body:?}").contains("Сводка"));
        let mut with_extra = serde_json::to_value(&body).unwrap();
        with_extra["source_revision"] = json!("worker-selected-source");
        assert!(WorkerGeneratedSummary::parse(&serde_json::to_vec(&with_extra).unwrap()).is_err());
        assert!(WorkerGeneratedSummary::parse(br#"["a","b","c","d","e","f",null]"#).is_err());
    }

    #[test]
    fn budget_counts_exact_upload_and_each_decoded_document_separately() {
        let mut body = body();
        body.markdown = "🦀".repeat(MAX_DOCUMENT_BYTES / 4);
        body.structured_json = json!("a".repeat(MAX_DOCUMENT_BYTES - 2));
        let mut bytes = serde_json::to_vec(&body).unwrap();
        bytes.resize(MAX_WORKER_SUMMARY_BODY_BYTES as usize, b' ');
        assert!(WorkerGeneratedSummary::parse(&bytes).is_ok());
        bytes.push(b' ');
        assert!(WorkerGeneratedSummary::parse(&bytes).is_err());
        body.markdown.push(' ');
        assert!(!body.valid());
        body.markdown.clear();
        body.structured_json = json!("a".repeat(MAX_DOCUMENT_BYTES - 1));
        assert!(!body.valid());
    }
}
