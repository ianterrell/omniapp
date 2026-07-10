use serde_yaml::{Mapping, Value};

use crate::WorkspaceError;

pub(crate) fn update_mapping(
    source: &str,
    updates: &[(String, Option<Value>)],
) -> Result<String, WorkspaceError> {
    parse_mapping(source)?;
    let mut result = source.to_owned();
    for (key, value) in updates {
        result = update_one(&result, key, value.as_ref())?;
    }
    parse_mapping(&result)?;
    Ok(result)
}

fn update_one(source: &str, key: &str, value: Option<&Value>) -> Result<String, WorkspaceError> {
    let spans = key_spans(source);
    if let Some(span) = spans.iter().find(|span| span.key == key) {
        let replacement = value
            .map(|value| render_entry(key, value, span.inline_comment.as_deref()))
            .transpose()?
            .unwrap_or_default();
        let mut result = String::with_capacity(source.len() + replacement.len());
        result.push_str(&source[..span.start]);
        result.push_str(&replacement);
        result.push_str(&source[span.end..]);
        return Ok(result);
    }
    let Some(value) = value else {
        return Ok(source.to_owned());
    };
    let mut result = source.to_owned();
    if !result.is_empty() && !result.ends_with('\n') {
        result.push('\n');
    }
    result.push_str(&render_entry(key, value, None)?);
    Ok(result)
}

fn render_entry(
    key: &str,
    value: &Value,
    inline_comment: Option<&str>,
) -> Result<String, WorkspaceError> {
    let mut mapping = Mapping::new();
    mapping.insert(Value::String(key.to_owned()), value.clone());
    let mut rendered = serde_yaml::to_string(&mapping).map_err(|error| {
        WorkspaceError::Invalid(format!("could not serialize YAML key {key:?}: {error}"))
    })?;
    if let Some(comment) = inline_comment
        && let Some(newline) = rendered.find('\n')
        && !rendered[..newline].trim_end().ends_with(':')
    {
        rendered.insert_str(newline, comment);
    }
    Ok(rendered)
}

fn parse_mapping(source: &str) -> Result<Mapping, WorkspaceError> {
    if source.trim().is_empty() {
        return Ok(Mapping::new());
    }
    serde_yaml::from_str::<Value>(source)
        .map_err(|error| WorkspaceError::Invalid(format!("could not parse YAML: {error}")))?
        .as_mapping()
        .cloned()
        .ok_or_else(|| WorkspaceError::Invalid("YAML document must contain a mapping".into()))
}

#[derive(Debug)]
struct KeySpan {
    key: String,
    start: usize,
    end: usize,
    inline_comment: Option<String>,
}

fn key_spans(source: &str) -> Vec<KeySpan> {
    let lines = source
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, line))
        })
        .collect::<Vec<_>>();
    let keys = lines
        .iter()
        .enumerate()
        .filter_map(|(index, (offset, line))| {
            top_level_key(line).map(|(key, comment)| (index, *offset, key, comment))
        })
        .collect::<Vec<_>>();
    keys.iter()
        .enumerate()
        .map(|(position, (line_index, start, key, comment))| {
            let next_key_line = keys
                .get(position + 1)
                .map_or(lines.len(), |(index, ..)| *index);
            let mut end = source.len();
            for (offset, line) in &lines[line_index + 1..next_key_line] {
                if is_top_level_separator(line) {
                    end = *offset;
                    break;
                }
            }
            if end == source.len()
                && let Some((_, next_offset, ..)) = keys.get(position + 1)
            {
                end = *next_offset;
            }
            KeySpan {
                key: key.clone(),
                start: *start,
                end,
                inline_comment: comment.clone(),
            }
        })
        .collect()
}

fn top_level_key(line: &str) -> Option<(String, Option<String>)> {
    let content = line.trim_end_matches(['\r', '\n']);
    if content.is_empty()
        || content.starts_with(char::is_whitespace)
        || content.starts_with('#')
        || content.starts_with(['-', '?', '%'])
    {
        return None;
    }
    let colon = structural_character(content, ':')?;
    if content[colon + 1..]
        .chars()
        .next()
        .is_some_and(|character| !character.is_whitespace())
    {
        return None;
    }
    let key_source = &content[..colon];
    let probe = format!("{key_source}: null\n");
    let key = serde_yaml::from_str::<Mapping>(&probe)
        .ok()?
        .into_iter()
        .next()?
        .0
        .as_str()?
        .to_owned();
    let comment = structural_character(&content[colon + 1..], '#').map(|index| {
        let absolute = colon + 1 + index;
        format!(" {}", content[absolute..].trim_start())
    });
    Some((key, comment))
}

fn structural_character(source: &str, needle: char) -> Option<usize> {
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut escaped = false;
    for (index, character) in source.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if double_quoted && character == '\\' {
            escaped = true;
            continue;
        }
        match character {
            '\'' if !double_quoted => single_quoted = !single_quoted,
            '"' if !single_quoted => double_quoted = !double_quoted,
            _ if character == needle && !single_quoted && !double_quoted => return Some(index),
            _ => {}
        }
    }
    None
}

fn is_top_level_separator(line: &str) -> bool {
    let content = line.trim_end_matches(['\r', '\n']);
    content.is_empty() || content.starts_with('#')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_only_targeted_top_level_entries() {
        let source = "# header\ntitle: \"Old\" # visible label\n\n# untouched block\nmetadata:\n  nested: true # keep nested\nstatus: draft\n";
        let updated = update_mapping(
            source,
            &[
                ("title".into(), Some(Value::String("New".into()))),
                ("status".into(), None),
                (
                    "tags".into(),
                    Some(Value::Sequence(vec![Value::String("rust".into())])),
                ),
            ],
        )
        .unwrap();
        assert!(updated.starts_with("# header\ntitle: New # visible label\n"));
        assert!(updated.contains("# untouched block\nmetadata:\n  nested: true # keep nested\n"));
        assert!(!updated.contains("status:"));
        assert!(updated.ends_with("tags:\n- rust\n"));
    }
}
