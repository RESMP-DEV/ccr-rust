//! Tests that every shipped example configuration still loads.
//!
//! Each `${VAR}` placeholder found in an example file is set to a dummy
//! value for the load and removed afterward, so a field rename or loader
//! change cannot silently rot the templates.

use super::*;

/// Every shipped example config must stay loadable by the real
/// `Config::from_file` path (env expansion, credential gate, provider
/// contracts, model aliases, retry-sweeps validation), so a field rename
/// or loader change cannot silently rot the templates. Each `${VAR}`
/// placeholder found in the file is set to a dummy value for the load
/// and removed afterward; names are real-world (ZAI_API_KEY, ...) so the
/// test deliberately does not run concurrently with other env-touching
/// configs, matching the precedent modules above.
#[test]
fn examples_configs_load_with_placeholder_env() {
    let examples_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let entries =
        std::fs::read_dir(&examples_dir).expect("examples/ directory must exist in the repo");
    let mut checked = 0usize;
    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut vars: Vec<String> = Vec::new();
        let mut rest = raw.as_str();
        while let Some(start) = rest.find("${") {
            rest = &rest[start + 2..];
            if let Some(end) = rest.find('}') {
                vars.push(rest[..end].to_string());
                rest = &rest[end + 1..];
            }
        }
        for var in &vars {
            std::env::set_var(var, "sk-example-placeholder");
        }
        let loaded = Config::from_file(path.to_str().unwrap());
        for var in &vars {
            std::env::remove_var(var);
        }
        let config =
            loaded.unwrap_or_else(|e| panic!("example {} must load: {:#}", path.display(), e));
        assert!(
            !config.providers().is_empty(),
            "example {} must declare providers",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 4,
        "expected the shipped example configs under examples/, found {checked}"
    );
}
