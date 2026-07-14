//! Audit logging for the broker (GĐ2, AC4).
//!
//! SECURITY (Hard Rule 1): this API **cannot** log a token — by design it takes
//! no token parameter. Every broker request is audited with only
//! tool/argv/decision/reason. Token values live in `ResolvedToken`/`BrokerResponse`
//! and never reach this module.

/// Build the audit line as a plain string (separated from I/O so it can be
/// asserted in tests). Never contains a token — the signature has no token arg.
pub fn format_audit_line(
    run_id: &str,
    tool: &str,
    argv: &[String],
    decision: &str,
    reason: Option<&str>,
) -> String {
    let argv_joined = argv.join(" ");
    match reason {
        Some(r) => format!(
            "broker run={run_id} tool={tool} argv=[{argv_joined}] decision={decision} reason={r}"
        ),
        None => format!(
            "broker run={run_id} tool={tool} argv=[{argv_joined}] decision={decision}"
        ),
    }
}

/// Emit an audit record for a broker request. Token is NEVER passed in.
pub fn audit_request(
    run_id: &str,
    tool: &str,
    argv: &[String],
    decision: &str,
    reason: Option<&str>,
) {
    let line = format_audit_line(run_id, tool, argv, decision, reason);
    tracing::info!(target: "devdy::broker::audit", "{line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_line_never_contains_token_word() {
        // Even if argv were to contain a token-looking value, the function has no
        // way to receive the resolved token; assert the formatted line only holds
        // what we passed and no "token=" field is produced.
        let argv = vec!["pr".to_string(), "create".to_string()];
        let line = format_audit_line("run-1", "gh", &argv, "ask", Some("write op"));
        assert!(line.contains("decision=ask"));
        assert!(line.contains("tool=gh"));
        assert!(!line.contains("token="), "audit line must not carry a token field");
    }

    #[test]
    fn audit_line_without_reason() {
        let argv = vec!["pr".to_string(), "list".to_string()];
        let line = format_audit_line("run-1", "gh", &argv, "allow", None);
        assert!(line.contains("decision=allow"));
        assert!(!line.contains("reason="));
        assert!(!line.contains("token="));
    }
}
