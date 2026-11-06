//! Builtin meeting templates and their file format, without inference or Tauri.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuiltinTemplate {
    pub id: &'static str,
    pub name: String,
    pub description: String,
    pub body: String,
}

const RAW: &[(&str, &str)] = &[
    ("general", include_str!("../templates/general.md")),
    ("sales-call", include_str!("../templates/sales-call.md")),
    (
        "client-discovery",
        include_str!("../templates/client-discovery.md"),
    ),
    ("one-on-one", include_str!("../templates/one-on-one.md")),
    ("standup", include_str!("../templates/standup.md")),
    ("interview", include_str!("../templates/interview.md")),
];

/// Parse `# Name`, `> description`, body.
pub fn parse_template_file(id: &'static str, src: &str) -> BuiltinTemplate {
    let mut name = id.to_string();
    let mut description = String::new();
    let mut body_lines = Vec::new();
    let mut header = true;
    for line in src.lines() {
        if header {
            if let Some(n) = line.strip_prefix("# ") {
                name = n.trim().to_string();
                continue;
            }
            if let Some(d) = line.strip_prefix("> ") {
                description = d.trim().to_string();
                continue;
            }
            if line.trim().is_empty() {
                continue;
            }
            header = false;
        }
        body_lines.push(line);
    }
    BuiltinTemplate {
        id,
        name,
        description,
        body: body_lines.join("\n").trim().to_string(),
    }
}

pub fn builtin_templates() -> Vec<BuiltinTemplate> {
    RAW.iter()
        .map(|(id, src)| parse_template_file(id, src))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_ids_order_and_defaults_match_existing_library_seed() {
        let templates = builtin_templates();
        assert_eq!(
            templates
                .iter()
                .map(|template| template.id)
                .collect::<Vec<_>>(),
            [
                "general",
                "sales-call",
                "client-discovery",
                "one-on-one",
                "standup",
                "interview"
            ]
        );
        assert_eq!(templates[0].name, "General meeting");
        assert_eq!(templates[0].description, "Balanced notes for any meeting: summary, decisions, action items, open questions, key moments.");
        for template in &templates {
            assert!(!template.name.is_empty(), "{}", template.id);
            assert!(!template.description.is_empty(), "{}", template.id);
            assert!(template.body.contains("{{transcript}}"), "{}", template.id);
            assert!(!template.body.starts_with('#'), "{}", template.id);
        }
        assert!(templates[0].body.ends_with("Transcript:\n{{transcript}}"));
    }

    #[test]
    fn headers_are_only_metadata_before_the_body_starts() {
        let parsed = parse_template_file(
            "custom",
            "\n#  Название  \r\n>  Описание  \r\n\r\nKeep {{transcript}}.\r\n\r\n# Body heading\r\n> Body quote\r\n",
        );
        assert_eq!(
            parsed,
            BuiltinTemplate {
                id: "custom",
                name: "Название".into(),
                description: "Описание".into(),
                body: "Keep {{transcript}}.\n\n# Body heading\n> Body quote".into(),
            }
        );
    }

    #[test]
    fn missing_metadata_retains_id_and_empty_description() {
        assert_eq!(
            parse_template_file("custom", "  Plain body\n\nSecond paragraph  \n"),
            BuiltinTemplate {
                id: "custom",
                name: "custom".into(),
                description: String::new(),
                body: "Plain body\n\nSecond paragraph".into(),
            }
        );
        assert_eq!(parse_template_file("empty", "\n \n").body, "");
    }
}
