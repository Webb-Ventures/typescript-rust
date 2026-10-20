use std::process::Command;

ts_r_testing::cap_memory!(64);

#[test]
fn small_allocations_are_fine() {
    let v = vec![0u8; 8 << 20];
    assert_eq!(v.len(), 8 << 20);
}

#[test]
fn runaway_allocation_aborts() {
    if std::env::var_os("TS_R_TESTING_CHILD").is_some() {
        let mut keep = Vec::new();
        loop {
            keep.push(vec![1u8; 1 << 20]);
        }
    }
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args(["--exact", "runaway_allocation_aborts", "--nocapture"])
        .env("TS_R_TESTING_CHILD", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("memory cap exceeded"),
        "status {:?}, stderr: {err}",
        out.status
    );
}
