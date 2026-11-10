//! The type-aware config loader stays serial across reloads and skips obsolete requests.

use std::fs;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use ts_r_lsp::lint::{ConfigSink, TypedConfigs};

ts_r_testing::cap_memory!();

const T: Duration = Duration::from_secs(10);

#[test]
fn reloads_share_one_loader_and_skip_obsolete_requests() {
    let root = std::env::temp_dir().join(format!("tsr-typed-configs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let config = root.join(".oxlintrc.json");
    fs::write(
        &config,
        r#"{"categories":{"correctness":"off"},"rules":{"typescript/no-unnecessary-type-assertion":"error"}}"#,
    )
    .unwrap();
    let path = root.join("a.ts").to_string_lossy().replace('\\', "/");
    let (tx, rx) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let wait = Mutex::new(wait);
    let sink: ConfigSink = Arc::new(move |generation, path, config| {
        tx.send((generation, path, config)).unwrap();
        if generation == 0 {
            // Hold the actual loader busy while the project reloads and queues more paths.
            wait.lock().unwrap().recv_timeout(T).unwrap();
        }
    });
    let mut configs = TypedConfigs::default();
    configs.request(&root, &path, &sink);
    let old = rx.recv_timeout(T).expect("first config loaded");
    assert!(old.2.is_some());
    configs.request(
        &root,
        &root
            .join("obsolete.ts")
            .to_string_lossy()
            .replace('\\', "/"),
        &sink,
    );
    // Every generation asks for a config while the first loader is still occupied.
    for _ in 0..3 {
        configs.reload();
        configs.request(&root, &path, &sink);
    }
    assert!(!configs.deliver(old.0, old.1, old.2));
    assert!(!configs.is_known(&path));
    // The retained loader must also forget its Engine's cached config on reload.
    fs::write(
        &config,
        r#"{"categories":{"correctness":"off"},"rules":{}}"#,
    )
    .unwrap();
    release.send(()).unwrap();
    let latest = rx.recv_timeout(T).expect("latest config loaded");
    assert_eq!(latest.0, 3, "an obsolete request ran");
    assert_eq!(latest.1, path);
    assert!(latest.2.is_none(), "the loader reused its old config");
    assert!(configs.deliver(latest.0, latest.1, latest.2));
    assert!(configs.is_known(&path));
    drop(configs);
    drop(sink);
    assert!(matches!(
        rx.recv_timeout(T),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    fs::remove_dir_all(root).unwrap();
}
