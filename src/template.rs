use crate::Result;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteContext {
    Unquoted,
    Single,
    Double,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Placeholder {
    start: usize,
    end: usize,
    name: String,
    context: QuoteContext,
}

pub(crate) fn parameters(template: &str) -> Result<Vec<String>> {
    let parameters: BTreeSet<String> = parse_placeholders(template)?
        .into_iter()
        .map(|placeholder| placeholder.name)
        .collect();
    Ok(parameters.into_iter().collect())
}

pub(crate) fn parse_arguments<'a>(
    values: impl IntoIterator<Item = &'a str>,
) -> Result<BTreeMap<String, String>> {
    let mut arguments = BTreeMap::new();
    for value in values {
        let (name, value) = value
            .split_once('=')
            .ok_or_else(|| format!("Render arguments must use NAME=VALUE syntax: '{value}'."))?;
        validate_parameter_name(name)?;
        if arguments
            .insert(name.to_string(), value.to_string())
            .is_some()
        {
            return Err(format!("Render argument '{name}' was provided more than once.").into());
        }
    }
    Ok(arguments)
}

pub(crate) fn render(template: &str, arguments: &BTreeMap<String, String>) -> Result<String> {
    let placeholders = parse_placeholders(template)?;
    let required: BTreeSet<&str> = placeholders
        .iter()
        .map(|placeholder| placeholder.name.as_str())
        .collect();

    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|name| !arguments.contains_key(*name))
        .collect();
    if !missing.is_empty() {
        return Err(format!("Missing render arguments: {}.", missing.join(", ")).into());
    }

    let unknown: Vec<&str> = arguments
        .keys()
        .map(String::as_str)
        .filter(|name| !required.contains(name))
        .collect();
    if !unknown.is_empty() {
        return Err(format!("Unknown render arguments: {}.", unknown.join(", ")).into());
    }

    let mut rendered = String::with_capacity(template.len());
    let mut cursor = 0;
    for placeholder in placeholders {
        rendered.push_str(&template[cursor..placeholder.start]);
        let value = arguments
            .get(&placeholder.name)
            .expect("render arguments were validated");
        match placeholder.context {
            QuoteContext::Unquoted => rendered.push_str(&shell_words::quote(value)),
            QuoteContext::Single => rendered.push_str(&escape_single_quoted_fragment(value)),
            QuoteContext::Double => rendered.push_str(&escape_double_quoted_fragment(value)),
        }
        cursor = placeholder.end;
    }
    rendered.push_str(&template[cursor..]);
    Ok(rendered)
}

fn parse_placeholders(template: &str) -> Result<Vec<Placeholder>> {
    let bytes = template.as_bytes();
    let mut placeholders = Vec::new();
    let mut context = QuoteContext::Unquoted;
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'\\' && context != QuoteContext::Single {
            if index + 1 >= bytes.len() {
                return Err("Template ends with an incomplete shell escape.".into());
            }
            index += 2;
            continue;
        }

        match bytes[index] {
            b'\'' if context == QuoteContext::Unquoted => {
                context = QuoteContext::Single;
                index += 1;
                continue;
            }
            b'\'' if context == QuoteContext::Single => {
                context = QuoteContext::Unquoted;
                index += 1;
                continue;
            }
            b'"' if context == QuoteContext::Unquoted => {
                context = QuoteContext::Double;
                index += 1;
                continue;
            }
            b'"' if context == QuoteContext::Double => {
                context = QuoteContext::Unquoted;
                index += 1;
                continue;
            }
            _ => {}
        }

        if bytes[index..].starts_with(b"{{") {
            let content_start = index + 2;
            let Some(relative_end) = template[content_start..].find("}}") else {
                return Err("Template contains an unclosed '{{' placeholder.".into());
            };
            let content_end = content_start + relative_end;
            let name = &template[content_start..content_end];
            validate_parameter_name(name)?;
            placeholders.push(Placeholder {
                start: index,
                end: content_end + 2,
                name: name.to_string(),
                context,
            });
            index = content_end + 2;
            continue;
        }

        if bytes[index..].starts_with(b"}}") {
            return Err("Template contains an unmatched '}}' placeholder terminator.".into());
        }

        index += template[index..]
            .chars()
            .next()
            .expect("template index is within bounds")
            .len_utf8();
    }

    match context {
        QuoteContext::Unquoted => Ok(placeholders),
        QuoteContext::Single => Err("Template contains an unclosed single quote.".into()),
        QuoteContext::Double => Err("Template contains an unclosed double quote.".into()),
    }
}

fn validate_parameter_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase())
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'));

    if valid {
        Ok(())
    } else {
        Err(format!(
            "Invalid template parameter '{name}'. Use lowercase letters, numbers, underscores, or hyphens, starting with a letter."
        )
        .into())
    }
}

fn escape_single_quoted_fragment(value: &str) -> String {
    value.replace('\'', "'\\''")
}

fn escape_double_quoted_fragment(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '"' | '$' | '`') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::{parameters, parse_arguments, render};
    use std::collections::BTreeMap;

    fn args(values: &[(&str, &str)]) -> BTreeMap<String, String> {
        values
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn discovers_sorted_deduplicated_parameters() {
        assert_eq!(
            parameters("curl {{url}} --header '{{header}}' {{url}}").unwrap(),
            vec!["header".to_string(), "url".to_string()]
        );
    }

    #[test]
    fn renders_values_safely_in_each_quote_context() {
        let rendered = render(
            "printf {{plain}} '{{single}}' \"{{double}}\"",
            &args(&[
                ("plain", "one; echo unsafe"),
                ("single", "O'Reilly"),
                ("double", "$(echo unsafe) `echo unsafe` \"quoted\""),
            ]),
        )
        .unwrap();

        assert_eq!(
            rendered,
            r#"printf 'one; echo unsafe' 'O'\''Reilly' "\$(echo unsafe) \`echo unsafe\` \"quoted\"""#
        );
    }

    #[test]
    fn rejects_missing_unknown_and_duplicate_arguments() {
        assert!(render("echo {{value}}", &BTreeMap::new())
            .unwrap_err()
            .to_string()
            .contains("Missing"));
        assert!(render("echo", &args(&[("value", "x")]))
            .unwrap_err()
            .to_string()
            .contains("Unknown"));
        assert!(parse_arguments(["value=one", "value=two"])
            .unwrap_err()
            .to_string()
            .contains("more than once"));
    }

    #[test]
    fn preserves_equals_signs_in_argument_values() {
        let parsed = parse_arguments(["query=a=b=c"]).unwrap();
        assert_eq!(parsed["query"], "a=b=c");
    }

    #[test]
    fn rejects_malformed_templates() {
        for template in ["echo {{value", "echo }}", "echo '{{value}}", "echo \\"] {
            assert!(parameters(template).is_err(), "{template}");
        }
    }
}
