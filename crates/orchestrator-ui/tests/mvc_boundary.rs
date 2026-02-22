use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;

fn collect_rs_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).expect("read_dir");
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, files);
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

#[test]
fn ui_manifest_avoids_direct_business_crates() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("manifest text");
    let parsed: Value = manifest.parse().expect("parse Cargo.toml");

    for forbidden in [
        "orchestrator-core",
        "orchestrator-worker-protocol",
        "orchestrator-ticketing",
        "orchestrator-harness",
        "orchestrator-vcs",
        "orchestrator-vcs-repos",
    ] {
        assert!(
            !manifest_declares_dependency(&parsed, forbidden),
            "orchestrator-ui must not directly depend on {forbidden}"
        );
    }
}

#[test]
fn ui_sources_use_app_frontend_boundary_only() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect_rs_files(&root.join("src"), &mut files);

    for file in files {
        let text = fs::read_to_string(&file).expect("source text");
        let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
        for forbidden in [
            "orchestrator_core::",
            "orchestrator_worker_protocol::",
            "orchestrator_ticketing::",
            "orchestrator_app::controller::",
        ] {
            assert!(
                !compact.contains(forbidden),
                "{} contains forbidden boundary import/reference: {forbidden}",
                file.display()
            );
        }

        let mut search_start = 0usize;
        while let Some(relative_idx) = compact[search_start..].find("orchestrator_app::") {
            let index = search_start + relative_idx;
            let remaining = &compact[index..];
            assert!(
                remaining.starts_with("orchestrator_app::frontend::ui_boundary"),
                "{} contains non-boundary orchestrator_app import/reference near: {}",
                file.display(),
                &remaining[..remaining.len().min(64)]
            );
            search_start = index + "orchestrator_app::".len();
        }
    }
}

fn manifest_declares_dependency(manifest: &Value, forbidden: &str) -> bool {
    if declares_dependency_in_tables(manifest, forbidden) {
        return true;
    }

    let Some(targets) = manifest.get("target").and_then(Value::as_table) else {
        return false;
    };

    for target in targets.values() {
        if declares_dependency_in_tables(target, forbidden) {
            return true;
        }
    }
    false
}

fn declares_dependency_in_tables(table_owner: &Value, forbidden: &str) -> bool {
    for table_name in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(table) = table_owner.get(table_name).and_then(Value::as_table) else {
            continue;
        };
        for (name, spec) in table {
            if name == forbidden {
                return true;
            }
            if spec
                .as_table()
                .and_then(|inline| inline.get("package"))
                .and_then(Value::as_str)
                == Some(forbidden)
            {
                return true;
            }
        }
    }
    false
}
