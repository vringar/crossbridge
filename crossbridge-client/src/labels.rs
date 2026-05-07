//! Helpers for working with the `xb*` label conventions.

/// Crossbridge label markers — kept as constants so the prefix shape is
/// asserted in tests and can't drift between submit/answer paths.
pub const INBOUND: &str = "xb:inbound";
pub const OUTBOUND: &str = "xb:outbound";
pub const STATUS_PENDING: &str = "xb-status:pending";
pub const STATUS_ANSWERED: &str = "xb-status:answered";
pub const SOURCE_PREFIX: &str = "xb-source:";
pub const REF_PREFIX: &str = "xb-ref:";

/// Format `xb-ref:<value>` for use as a label.
#[must_use]
pub fn ref_label(value: &str) -> String {
    format!("{REF_PREFIX}{value}")
}

/// Find the value after `prefix` in the first matching label.
#[must_use]
pub fn find_prefixed<'a>(labels: &'a [String], prefix: &str) -> Option<&'a str> {
    labels.iter().find_map(|l| l.strip_prefix(prefix))
}

/// True if any label exactly matches `marker`.
#[must_use]
pub fn has(labels: &[String], marker: &str) -> bool {
    labels.iter().any(|l| l == marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn finds_source_and_ref() {
        let labels = s(&[
            "type:request",
            "xb:inbound",
            "xb-source:firmware",
            "xb-ref:abc-123",
        ]);
        assert_eq!(find_prefixed(&labels, SOURCE_PREFIX), Some("firmware"));
        assert_eq!(find_prefixed(&labels, REF_PREFIX), Some("abc-123"));
    }

    #[test]
    fn missing_prefix_yields_none() {
        let labels = s(&["xb:inbound"]);
        assert_eq!(find_prefixed(&labels, SOURCE_PREFIX), None);
    }

    #[test]
    fn has_marker() {
        let labels = s(&["xb:inbound", "type:request"]);
        assert!(has(&labels, INBOUND));
        assert!(!has(&labels, OUTBOUND));
    }

    #[test]
    fn ref_label_format() {
        assert_eq!(ref_label("42"), "xb-ref:42");
    }
}
