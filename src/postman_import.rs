use crate::config::validate_shelf_name;
use crate::database::CommandDatabase;
use crate::Result;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PostmanImportWarning {
    pub(crate) request_name: String,
    pub(crate) reason: String,
}

#[derive(Debug, PartialEq)]
pub(crate) struct PostmanImportOutcome {
    pub(crate) shelf_name: String,
    pub(crate) database: CommandDatabase,
    pub(crate) warnings: Vec<PostmanImportWarning>,
}

#[derive(Debug, Deserialize)]
struct PostmanCollection {
    info: PostmanCollectionInfo,
    #[serde(default)]
    item: Vec<PostmanItem>,
    #[serde(default)]
    auth: Option<Value>,
    #[serde(default)]
    event: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct PostmanCollectionInfo {
    name: Option<String>,
    schema: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PostmanItem {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    item: Vec<PostmanItem>,
    #[serde(default)]
    request: Option<PostmanRequest>,
    #[serde(default)]
    auth: Option<Value>,
    #[serde(default)]
    event: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct PostmanRequest {
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    header: Vec<PostmanHeader>,
    #[serde(default)]
    body: Option<PostmanBody>,
    #[serde(default)]
    url: Option<PostmanUrl>,
    #[serde(default)]
    auth: Option<Value>,
    #[serde(default)]
    event: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct PostmanHeader {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    disabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct PostmanBody {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    raw: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PostmanUrl {
    Raw(String),
    Detailed { raw: Option<String> },
}

#[derive(Debug, Clone, Copy)]
struct UnsupportedContext {
    auth: bool,
    scripts: bool,
}

impl UnsupportedContext {
    fn next(self, auth: bool, scripts: bool) -> Self {
        Self {
            auth: self.auth || auth,
            scripts: self.scripts || scripts,
        }
    }
}

pub(crate) fn import_postman_collection(
    path: &Path,
    shelf_override: Option<&str>,
) -> Result<PostmanImportOutcome> {
    let content = std::fs::read_to_string(path)?;
    import_postman_collection_from_str(&content, shelf_override)
}

fn import_postman_collection_from_str(
    content: &str,
    shelf_override: Option<&str>,
) -> Result<PostmanImportOutcome> {
    let collection: PostmanCollection = serde_json::from_str(content)
        .map_err(|error| format!("Failed to parse Postman collection JSON: {error}"))?;

    validate_collection_schema(&collection)?;

    let shelf_name = resolve_shelf_name(&collection, shelf_override)?;
    let mut database = CommandDatabase::new();
    let mut warnings = Vec::new();
    let initial_context = UnsupportedContext {
        auth: collection.auth.is_some(),
        scripts: !collection.event.is_empty(),
    };

    import_items(
        &collection.item,
        initial_context,
        &mut database,
        &mut warnings,
    );

    if database.commands.is_empty() {
        return Err(format_all_skipped_error(warnings).into());
    }

    Ok(PostmanImportOutcome {
        shelf_name,
        database,
        warnings,
    })
}

fn validate_collection_schema(collection: &PostmanCollection) -> Result<()> {
    let Some(schema) = collection.info.schema.as_deref() else {
        return Err("Unsupported Postman collection schema: missing info.schema.".into());
    };

    if schema.contains("/collection/v2.1") {
        Ok(())
    } else {
        Err(format!(
            "Unsupported Postman collection schema '{schema}'. Expected Collection v2.1 export."
        )
        .into())
    }
}

fn resolve_shelf_name(
    collection: &PostmanCollection,
    shelf_override: Option<&str>,
) -> Result<String> {
    let shelf_name = match shelf_override {
        Some(shelf) => shelf.to_string(),
        None => collection
            .info
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or("Postman collection is missing a usable info.name for shelf creation.")?
            .to_string(),
    };

    validate_shelf_name(&shelf_name)?;
    Ok(shelf_name)
}

fn import_items(
    items: &[PostmanItem],
    context: UnsupportedContext,
    database: &mut CommandDatabase,
    warnings: &mut Vec<PostmanImportWarning>,
) {
    for item in items {
        let item_context = context.next(item.auth.is_some(), !item.event.is_empty());

        if let Some(request) = item.request.as_ref() {
            import_request(item, request, item_context, database, warnings);
        }

        if !item.item.is_empty() {
            import_items(&item.item, item_context, database, warnings);
        }
    }
}

fn import_request(
    item: &PostmanItem,
    request: &PostmanRequest,
    context: UnsupportedContext,
    database: &mut CommandDatabase,
    warnings: &mut Vec<PostmanImportWarning>,
) {
    let request_name = item
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Unnamed request")
        .to_string();

    match convert_request_to_curl(request, context) {
        Ok(command) => {
            let description = item
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned);

            if !database.add_command(command, description) {
                warnings.push(PostmanImportWarning {
                    request_name,
                    reason: "generated a duplicate command".to_string(),
                });
            }
        }
        Err(reason) => warnings.push(PostmanImportWarning {
            request_name,
            reason,
        }),
    }
}

fn convert_request_to_curl(
    request: &PostmanRequest,
    context: UnsupportedContext,
) -> std::result::Result<String, String> {
    let request_context = context.next(request.auth.is_some(), !request.event.is_empty());
    if request_context.auth {
        return Err("uses auth or auth inheritance, which is not supported yet".to_string());
    }
    if request_context.scripts {
        return Err("uses scripts, which are not supported yet".to_string());
    }

    let method = request
        .method
        .as_deref()
        .map(str::trim)
        .filter(|method| !method.is_empty())
        .ok_or_else(|| "is missing an HTTP method".to_string())?;
    let url =
        extract_url(request.url.as_ref()).ok_or_else(|| "is missing a raw URL".to_string())?;

    let mut parts = vec!["curl".to_string()];
    if !method.eq_ignore_ascii_case("GET") {
        parts.push("-X".to_string());
        parts.push(shell_quote(method));
    }

    for header in enabled_headers(&request.header)? {
        parts.push("-H".to_string());
        parts.push(shell_quote(&header));
    }

    if let Some(body) = request.body.as_ref() {
        match body.mode.as_deref() {
            None => {}
            Some("raw") => {
                parts.push("--data-raw".to_string());
                parts.push(shell_quote(body.raw.as_deref().unwrap_or_default()));
            }
            Some(mode) => {
                return Err(format!("uses unsupported body mode '{mode}'"));
            }
        }
    }

    parts.push(shell_quote(&url));
    Ok(parts.join(" "))
}

fn enabled_headers(headers: &[PostmanHeader]) -> std::result::Result<Vec<String>, String> {
    let mut enabled = Vec::new();

    for header in headers {
        if header.disabled.unwrap_or(false) {
            continue;
        }

        let key = header
            .key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| "contains a header without a key".to_string())?;
        let value = header.value.as_deref().unwrap_or_default();

        if value.is_empty() {
            enabled.push(format!("{key}:"));
        } else {
            enabled.push(format!("{key}: {value}"));
        }
    }

    Ok(enabled)
}

fn extract_url(url: Option<&PostmanUrl>) -> Option<String> {
    match url? {
        PostmanUrl::Raw(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        PostmanUrl::Detailed { raw } => raw
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn format_all_skipped_error(warnings: Vec<PostmanImportWarning>) -> String {
    let mut lines = vec!["Postman import failed: no supported requests were found.".to_string()];

    if !warnings.is_empty() {
        lines.push("Skipped requests:".to_string());
        for warning in warnings {
            lines.push(format!("- {}: {}", warning.request_name, warning.reason));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        import_postman_collection_from_str, shell_quote, PostmanImportOutcome, PostmanImportWarning,
    };

    fn import_fixture(content: &str, shelf_override: Option<&str>) -> PostmanImportOutcome {
        import_postman_collection_from_str(content, shelf_override).expect("import should succeed")
    }

    #[test]
    fn test_imports_v21_collection_and_generates_curl() {
        let outcome = import_fixture(
            r#"{
  "info": {
    "name": "postman-api",
    "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
  },
  "item": [
    {
      "name": "Create workspace",
      "request": {
        "method": "POST",
        "header": [
          { "key": "Content-Type", "value": "application/json" },
          { "key": "X-Trace", "value": "{{traceId}}" }
        ],
        "body": {
          "mode": "raw",
          "raw": "{\"name\":\"demo\"}"
        },
        "url": {
          "raw": "https://api.getpostman.com/workspaces"
        }
      }
    }
  ]
}"#,
            None,
        );

        assert_eq!(outcome.shelf_name, "postman-api");
        assert!(outcome.warnings.is_empty());
        assert_eq!(outcome.database.commands.len(), 1);
        assert_eq!(
            outcome.database.commands[0].command,
            "curl -X 'POST' -H 'Content-Type: application/json' -H 'X-Trace: {{traceId}}' --data-raw '{\"name\":\"demo\"}' 'https://api.getpostman.com/workspaces'"
        );
        assert_eq!(
            outcome.database.commands[0].description.as_deref(),
            Some("Create workspace")
        );
    }

    #[test]
    fn test_recursively_flattens_folder_items() {
        let outcome = import_fixture(
            r#"{
  "info": {
    "name": "curl",
    "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
  },
  "item": [
    {
      "name": "Admin",
      "item": [
        {
          "name": "Health",
          "request": {
            "method": "GET",
            "url": "https://example.com/health"
          }
        },
        {
          "name": "Versions",
          "request": {
            "method": "GET",
            "url": {
              "raw": "https://example.com/versions"
            }
          }
        }
      ]
    }
  ]
}"#,
            None,
        );

        let commands: Vec<&str> = outcome
            .database
            .commands
            .iter()
            .map(|command| command.command.as_str())
            .collect();
        assert_eq!(
            commands,
            vec![
                "curl 'https://example.com/health'",
                "curl 'https://example.com/versions'"
            ]
        );
    }

    #[test]
    fn test_shelf_override_takes_precedence() {
        let outcome = import_fixture(
            r#"{
  "info": {
    "name": "ignored-name",
    "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
  },
  "item": [
    {
      "name": "Ping",
      "request": {
        "method": "GET",
        "url": "https://example.com/ping"
      }
    }
  ]
}"#,
            Some("curl"),
        );

        assert_eq!(outcome.shelf_name, "curl");
    }

    #[test]
    fn test_unsupported_requests_return_warnings() {
        let outcome = import_fixture(
            r#"{
  "info": {
    "name": "curl",
    "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
  },
  "item": [
    {
      "name": "Supported",
      "request": {
        "method": "GET",
        "url": "https://example.com/supported"
      }
    },
    {
      "name": "Upload file",
      "request": {
        "method": "POST",
        "body": {
          "mode": "formdata"
        },
        "url": "https://example.com/upload"
      }
    }
  ]
}"#,
            None,
        );

        assert_eq!(outcome.database.commands.len(), 1);
        assert_eq!(
            outcome.warnings,
            vec![PostmanImportWarning {
                request_name: "Upload file".to_string(),
                reason: "uses unsupported body mode 'formdata'".to_string(),
            }]
        );
    }

    #[test]
    fn test_all_skipped_collections_fail() {
        let error = import_postman_collection_from_str(
            r#"{
  "info": {
    "name": "curl",
    "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
  },
  "item": [
    {
      "name": "Secured request",
      "request": {
        "method": "GET",
        "auth": {
          "type": "bearer"
        },
        "url": "https://example.com/secure"
      }
    }
  ]
}"#,
            None,
        )
        .expect_err("collection should fail when every request is skipped");

        assert!(error
            .to_string()
            .contains("Postman import failed: no supported requests were found."));
        assert!(error.to_string().contains(
            "Secured request: uses auth or auth inheritance, which is not supported yet"
        ));
    }

    #[test]
    fn test_invalid_schema_is_rejected() {
        let error = import_postman_collection_from_str(
            r#"{
  "info": {
    "name": "curl",
    "schema": "https://schema.getpostman.com/json/collection/v2.0.0/collection.json"
  },
  "item": []
}"#,
            None,
        )
        .expect_err("schema should be rejected");

        assert_eq!(
            error.to_string(),
            "Unsupported Postman collection schema 'https://schema.getpostman.com/json/collection/v2.0.0/collection.json'. Expected Collection v2.1 export."
        );
    }

    #[test]
    fn test_invalid_collection_name_is_rejected() {
        let error = import_postman_collection_from_str(
            r#"{
  "info": {
    "name": "curl/api",
    "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
  },
  "item": []
}"#,
            None,
        )
        .expect_err("invalid shelf name should fail");

        assert_eq!(
            error.to_string(),
            "Shelf names may only contain letters, numbers, dots, underscores, and hyphens."
        );
    }

    #[test]
    fn test_shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    }
}
