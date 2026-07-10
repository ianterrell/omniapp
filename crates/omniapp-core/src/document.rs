use std::fs;
use std::path::Path;

use serde_yaml::{Mapping, Value};

use crate::WorkspaceError;

#[derive(Debug, Default)]
pub(crate) struct MarkdownDocument {
    pub frontmatter: Mapping,
    pub body: String,
    pub had_frontmatter: bool,
}

impl MarkdownDocument {
    pub fn read(path: &Path) -> Result<Self, WorkspaceError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(path).map_err(|source| WorkspaceError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&contents, path)
    }

    fn parse(contents: &str, path: &Path) -> Result<Self, WorkspaceError> {
        let mut lines = contents.split_inclusive('\n');
        let Some(first) = lines.next() else {
            return Ok(Self::default());
        };
        if first.trim_end_matches(['\r', '\n']) != "---" {
            return Ok(Self {
                body: contents.to_owned(),
                ..Self::default()
            });
        }

        let mut yaml = String::new();
        let mut consumed = first.len();
        let mut closed = false;
        for line in lines {
            consumed += line.len();
            if line.trim_end_matches(['\r', '\n']) == "---" {
                closed = true;
                break;
            }
            yaml.push_str(line);
        }
        if !closed {
            return Ok(Self {
                body: contents.to_owned(),
                ..Self::default()
            });
        }
        let frontmatter = if yaml.trim().is_empty() {
            Mapping::new()
        } else {
            serde_yaml::from_str::<Value>(&yaml)
                .map_err(|error| {
                    WorkspaceError::Invalid(format!(
                        "could not parse frontmatter in {}: {error}",
                        path.display()
                    ))
                })?
                .as_mapping()
                .cloned()
                .ok_or_else(|| {
                    WorkspaceError::Invalid(format!(
                        "frontmatter in {} must be a YAML mapping",
                        path.display()
                    ))
                })?
        };
        Ok(Self {
            frontmatter,
            body: contents[consumed..].to_owned(),
            had_frontmatter: true,
        })
    }

    pub fn render(&self, force_frontmatter: bool) -> Result<String, WorkspaceError> {
        if !force_frontmatter && !self.had_frontmatter && self.frontmatter.is_empty() {
            return Ok(self.body.clone());
        }
        let yaml = if self.frontmatter.is_empty() {
            String::new()
        } else {
            serde_yaml::to_string(&self.frontmatter).map_err(|error| {
                WorkspaceError::Invalid(format!(
                    "could not serialize Markdown frontmatter: {error}"
                ))
            })?
        };
        Ok(format!("---\n{yaml}---\n{}", self.body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_renders_frontmatter_without_losing_unknown_keys() {
        let input = "---\ntitle: Dune\ncustom: keep-me\n---\n# Body\n";
        let mut document = MarkdownDocument::parse(input, Path::new("post.md")).unwrap();
        assert_eq!(document.body, "# Body\n");
        document.frontmatter.insert(
            Value::String("title".into()),
            Value::String("Dune Messiah".into()),
        );
        let rendered = document.render(true).unwrap();
        assert!(rendered.contains("title: Dune Messiah"));
        assert!(rendered.contains("custom: keep-me"));
        assert!(rendered.ends_with("---\n# Body\n"));
    }

    #[test]
    fn ordinary_horizontal_rule_is_body_without_a_closing_delimiter() {
        let document = MarkdownDocument::parse("---\nbody", Path::new("post.md")).unwrap();
        assert!(!document.had_frontmatter);
        assert_eq!(document.body, "---\nbody");
    }
}
