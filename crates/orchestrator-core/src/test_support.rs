use std::cell::Cell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static ENV_VAR_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static TEST_PATH_COUNTER: AtomicU64 = AtomicU64::new(0);
thread_local! {
    static ENV_VAR_LOCK_DEPTH: Cell<u32> = const { Cell::new(0) };
}

struct EnvVarScope {
    _guard: Option<MutexGuard<'static, ()>>,
}

impl EnvVarScope {
    fn enter() -> Self {
        let depth_before = ENV_VAR_LOCK_DEPTH.with(|depth| {
            let current = depth.get();
            depth.set(current.saturating_add(1));
            current
        });

        if depth_before > 0 {
            return Self { _guard: None };
        }

        let lock = ENV_VAR_LOCK.get_or_init(|| Mutex::new(()));
        let guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        Self {
            _guard: Some(guard),
        }
    }
}

impl Drop for EnvVarScope {
    fn drop(&mut self) {
        ENV_VAR_LOCK_DEPTH.with(|depth| {
            let current = depth.get();
            depth.set(current.saturating_sub(1));
        });
    }
}

struct EnvVarRestore {
    key: String,
    original: Option<OsString>,
}

impl EnvVarRestore {
    fn new(key: &str, value: Option<&str>) -> Self {
        let original = std::env::var_os(key);
        match value {
            Some(value) => unsafe {
                std::env::set_var(key, value);
            },
            None => unsafe {
                std::env::remove_var(key);
            },
        }

        Self {
            key: key.to_owned(),
            original,
        }
    }
}

impl Drop for EnvVarRestore {
    fn drop(&mut self) {
        match self.original.take() {
            Some(original) => unsafe {
                std::env::set_var(&self.key, original);
            },
            None => unsafe {
                std::env::remove_var(&self.key);
            },
        }
    }
}

pub fn with_env_var<R>(key: &str, value: Option<&str>, run: impl FnOnce() -> R) -> R {
    with_env_vars(&[(key, value)], run)
}

pub fn with_env_vars<R>(vars: &[(&str, Option<&str>)], run: impl FnOnce() -> R) -> R {
    let _scope = EnvVarScope::enter();
    let _restores: Vec<_> = vars
        .iter()
        .map(|(key, value)| EnvVarRestore::new(key, *value))
        .collect();
    run()
}

pub fn unique_test_db_path(tag: &str) -> PathBuf {
    let safe_tag: String = tag
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = TEST_PATH_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "orchestrator-{safe_tag}-{}-{now_nanos}-{counter}.db",
        std::process::id(),
    ))
}

pub struct TestDbPath {
    path: PathBuf,
}

impl TestDbPath {
    pub fn new(tag: &str) -> Self {
        Self {
            path: unique_test_db_path(tag),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDbPath {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "warning: failed to remove temporary test database {}: {err}",
                    self.path.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static ENV_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_test_key(prefix: &str) -> String {
        let counter = ENV_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}_{}_{}", std::process::id(), counter)
    }

    #[test]
    fn with_env_var_restores_original_value() {
        let key = unique_test_key("ORCHESTRATOR_TEST_HELPER_ENV");

        with_env_var(&key, Some("before"), || {
            with_env_var(&key, Some("during"), || {
                assert_eq!(std::env::var(&key).expect("value during closure"), "during");
            });
            assert_eq!(std::env::var(&key).expect("restored value"), "before");
        });

        assert!(
            std::env::var(&key).is_err(),
            "expected helper to clean up key"
        );
    }

    #[test]
    fn with_env_var_restores_missing_value() {
        let key = unique_test_key("ORCHESTRATOR_TEST_HELPER_ENV_MISSING");

        with_env_var(&key, Some("temporary"), || {
            assert_eq!(
                std::env::var(&key).expect("value during closure"),
                "temporary"
            );
        });

        assert!(
            std::env::var(&key).is_err(),
            "expected helper to restore missing env var"
        );
    }

    #[test]
    fn unique_test_db_path_is_stable_and_unique() {
        let first = unique_test_db_path("helper");
        let second = unique_test_db_path("helper");

        assert_ne!(first, second);
        assert!(first.to_string_lossy().contains("orchestrator-helper-"));
        assert!(second.to_string_lossy().contains("orchestrator-helper-"));
        assert!(first.to_string_lossy().ends_with(".db"));
        assert!(second.to_string_lossy().ends_with(".db"));
    }

    #[test]
    fn with_env_vars_sets_and_restores_multiple_values() {
        let first = unique_test_key("ORCHESTRATOR_TEST_SUPPORT_FIRST");
        let second = unique_test_key("ORCHESTRATOR_TEST_SUPPORT_SECOND");

        with_env_vars(&[(&first, Some("one")), (&second, Some("two"))], || {
            assert_eq!(std::env::var(&first).expect("first value"), "one");
            assert_eq!(std::env::var(&second).expect("second value"), "two");
        });

        assert!(std::env::var(&first).is_err());
        assert!(std::env::var(&second).is_err());
    }

    #[test]
    fn test_db_path_cleans_up_created_file() {
        let lingering_path = {
            let temp_db = TestDbPath::new("cleanup-check");
            let path = temp_db.path().to_path_buf();
            std::fs::write(&path, "placeholder").expect("create test file");
            path
        };

        assert!(
            !lingering_path.exists(),
            "expected temporary test file to be cleaned on drop"
        );
    }

    #[test]
    fn with_env_var_supports_nested_usage_in_same_thread() {
        let outer_key = unique_test_key("ORCHESTRATOR_TEST_SUPPORT_OUTER");
        let inner_key = unique_test_key("ORCHESTRATOR_TEST_SUPPORT_INNER");

        with_env_var(&outer_key, Some("outer"), || {
            with_env_var(&inner_key, Some("inner"), || {
                assert_eq!(std::env::var(&outer_key).expect("outer value"), "outer");
                assert_eq!(std::env::var(&inner_key).expect("inner value"), "inner");
            });

            assert_eq!(std::env::var(&outer_key).expect("outer value"), "outer");
            assert!(std::env::var(&inner_key).is_err());
        });

        assert!(std::env::var(&outer_key).is_err());
        assert!(std::env::var(&inner_key).is_err());
    }
}
