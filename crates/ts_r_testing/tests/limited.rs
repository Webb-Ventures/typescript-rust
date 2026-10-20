//! `limited` on this platform: limit.sh on Unix, a Job Object on Windows. Each test re-runs
//! this binary as the child, so the same checks hold on both.

use ts_r_testing::{EXIT_MEMORY_LIMIT, EXIT_TIME_LIMIT, limited};

// The child allocates at most 1024 MB even if `limited` failed to stop it.
ts_r_testing::cap_memory!(1024);

const CHILD: &str = "TS_R_TESTING_LIMITED_CHILD";

/// What the child does, picked by `CHILD`; `None` in the parent.
fn child_role() -> Option<String> {
    std::env::var(CHILD).ok()
}

fn run_child(test: &str, role: &str, mem_mb: u32, time_s: u32) -> std::process::Output {
    limited(std::env::current_exe().expect("test exe"), mem_mb, time_s)
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, role)
        .output()
        .expect("run child")
}

#[test]
fn passes_output_and_exit_code_through() {
    if child_role().is_some() {
        println!("hello from the child");
        std::process::exit(3);
    }
    let out = run_child("passes_output_and_exit_code_through", "exit", 256, 60);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("hello from the child"));
}

#[test]
fn kills_on_time_limit() {
    if child_role().is_some() {
        std::thread::sleep(std::time::Duration::from_secs(30));
        return;
    }
    let started = std::time::Instant::now();
    let out = run_child("kills_on_time_limit", "sleep", 256, 1);
    assert_eq!(out.status.code(), Some(EXIT_TIME_LIMIT), "{out:?}");
    assert!(
        started.elapsed().as_secs() < 20,
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn kills_on_memory_limit() {
    if child_role().is_some() {
        // Touch every page so it counts as resident (limit.sh) and committed (Job Object).
        let mut keep = Vec::new();
        for _ in 0..768 {
            keep.push(vec![1u8; 1 << 20]);
        }
        std::thread::sleep(std::time::Duration::from_secs(30));
        drop(keep);
        return;
    }
    let out = run_child("kills_on_memory_limit", "alloc", 128, 60);
    assert_eq!(out.status.code(), Some(EXIT_MEMORY_LIMIT), "{out:?}");
}
