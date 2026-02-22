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
        "orchestrator-domain",
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
            "orchestrator_domain::",
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

#[test]
fn ui_lib_root_has_no_include_flattening() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib_rs = fs::read_to_string(root.join("src/lib.rs")).expect("lib.rs text");
    assert!(
        !lib_rs.contains("include!("),
        "orchestrator-ui crate root must not flatten modules with include!"
    );
}

#[test]
fn ui_domain_modules_are_declared() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ui_mod = fs::read_to_string(root.join("src/ui/mod.rs")).expect("ui/mod.rs text");

    for declaration in [
        "pub(crate) mod bootstrap;",
        "pub(crate) mod view_state;",
        "pub(crate) mod input;",
        "pub(crate) mod render;",
        "pub(crate) mod features;",
        "pub(crate) mod theme;",
    ] {
        assert!(
            ui_mod.contains(declaration),
            "expected UI domain module declaration missing: {declaration}"
        );
    }
}

#[test]
fn ui_sources_have_no_include_macros() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect_rs_files(&root.join("src"), &mut files);

    for file in files {
        let text = fs::read_to_string(&file).expect("source text");
        assert!(
            !text.contains("include!("),
            "{} must not use include!-based flattening",
            file.display()
        );
    }
}

#[test]
fn ui_legacy_flattening_fragments_are_removed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ui_root = root.join("src/ui");

    for legacy_fragment in [
        "types_and_projection.rs",
        "inspector_and_supervisor.rs",
        "focus_card_and_ticket_picker.rs",
        "shell_state.rs",
        "async_tasks_and_overlay_routing.rs",
        "runtime.rs",
        "rendering.rs",
        "input_and_terminal.rs",
    ] {
        assert!(
            !ui_root.join(legacy_fragment).exists(),
            "legacy include! fragment should be removed: {legacy_fragment}"
        );
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
