use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("crates/orchestrator should have a workspace root parent")
        .to_path_buf()
}

#[test]
fn workspace_manifest_lists_all_crates_and_excludes_removed_orchestrator_core() {
    let root = repo_root();
    let workspace_manifest =
        fs::read_to_string(root.join("Cargo.toml")).expect("read workspace Cargo.toml");

    assert!(
        !workspace_manifest.contains("crates/orchestrator-core"),
        "workspace manifest must not reference removed crates/orchestrator-core",
    );
    assert!(
        !root.join("crates/orchestrator-core").exists(),
        "crates/orchestrator-core directory should be removed from the workspace",
    );

    let crates_dir = root.join("crates");
    let entries = fs::read_dir(&crates_dir).expect("read crates directory");
    for entry in entries {
        let entry = entry.expect("read crate entry");
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !path.join("Cargo.toml").exists() {
            continue;
        }

        let crate_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("crate directory name must be valid UTF-8");
        let expected_member = format!("\"crates/{crate_name}\"");
        assert!(
            workspace_manifest.contains(&expected_member),
            "workspace manifest is missing member {expected_member}",
        );
    }
}

#[test]
fn crate_manifests_do_not_reference_orchestrator_core_dependency() {
    let root = repo_root();
    let crates_dir = root.join("crates");
    let entries = fs::read_dir(&crates_dir).expect("read crates directory");
    for entry in entries {
        let entry = entry.expect("read crate entry");
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("Cargo.toml");
        if !manifest_path.exists() {
            continue;
        }

        let manifest = fs::read_to_string(&manifest_path)
            .unwrap_or_else(|_| panic!("read {}", manifest_path.display()));
        assert!(
            !manifest.contains("orchestrator-core"),
            "{} still references removed orchestrator-core dependency",
            manifest_path.display(),
        );
    }
}
