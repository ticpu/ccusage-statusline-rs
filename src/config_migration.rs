use serde_json::Value;
use std::collections::HashSet;
use std::io::IsTerminal;

/// Schema version this binary writes. Bump with every entry added to MIGRATIONS.
pub const CURRENT_VERSION: u64 = 1;

/// Entry at index N migrates a document from version N to N+1. Never reorder or drop
/// one: a config file may sit at any version.
const MIGRATIONS: &[fn(&mut Value)] = &[sonnet_element_to_model_scoped];

/// Brings a raw config document up to CURRENT_VERSION. True when it changed and the
/// caller should persist it.
pub fn migrate(doc: &mut Value) -> bool {
    if !doc.is_object() {
        return false;
    }

    let from = doc
        .get("version")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    if from > CURRENT_VERSION {
        if std::io::stderr().is_terminal() {
            eprintln!(
                "config: written by a newer version (schema v{from}, this binary knows v{CURRENT_VERSION}); unknown settings are ignored"
            );
        }
        return false;
    }
    if from == CURRENT_VERSION {
        return false;
    }

    for step in &MIGRATIONS[from as usize..CURRENT_VERSION as usize] {
        step(doc);
    }
    doc["version"] = Value::from(CURRENT_VERSION);
    true
}

/// v0 -> v1: the Sonnet-only weekly element became the per-model one, which renders the
/// Sonnet window along with every other bucket the server reports.
fn sonnet_element_to_model_scoped(doc: &mut Value) {
    let Some(elements) = doc
        .get_mut("enabled_elements")
        .and_then(Value::as_array_mut)
    else {
        return;
    };

    for element in elements.iter_mut() {
        if element.as_str() == Some("api_metrics_sonnet") {
            *element = Value::from("api_metrics_model7d");
        }
    }

    let mut seen = HashSet::new();
    elements.retain(|element| {
        element
            .as_str()
            .is_none_or(|name| seen.insert(name.to_string()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elements(doc: &Value) -> Vec<String> {
        doc["enabled_elements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                e.as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn test_v0_renames_sonnet_element() {
        let mut doc: Value =
            serde_json::from_str(r#"{"enabled_elements": ["model", "api_metrics_sonnet"]}"#)
                .unwrap();
        assert!(migrate(&mut doc));
        assert_eq!(elements(&doc), ["model", "api_metrics_model7d"]);
        assert_eq!(doc["version"], Value::from(CURRENT_VERSION));
    }

    /// A config that already lists the per-model element must not end up with it twice.
    #[test]
    fn test_v0_rename_collapses_duplicate() {
        let mut doc: Value = serde_json::from_str(
            r#"{"enabled_elements": ["api_metrics_model7d", "api_metrics_sonnet"]}"#,
        )
        .unwrap();
        assert!(migrate(&mut doc));
        assert_eq!(elements(&doc), ["api_metrics_model7d"]);
    }

    #[test]
    fn test_current_version_is_untouched() {
        let json = format!(
            r#"{{"version": {CURRENT_VERSION}, "enabled_elements": ["api_metrics_sonnet"]}}"#
        );
        let mut doc: Value = serde_json::from_str(&json).unwrap();
        assert!(!migrate(&mut doc));
        assert_eq!(elements(&doc), ["api_metrics_sonnet"]);
    }

    /// A file from a future binary is left alone rather than downgraded in place.
    #[test]
    fn test_future_version_is_untouched() {
        let json = format!(
            r#"{{"version": {}, "enabled_elements": []}}"#,
            CURRENT_VERSION + 1
        );
        let mut doc: Value = serde_json::from_str(&json).unwrap();
        assert!(!migrate(&mut doc));
        assert_eq!(doc["version"], Value::from(CURRENT_VERSION + 1));
    }
}
