//! A small scoped worker pool (std threads with large stacks, since trees can be deep).

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

/// Stack size for workers: deep trees recurse in the analyzers.
const STACK: usize = 64 << 20;

/// Worker threads: `opts.threads` if non-zero, else `TS_R_THREADS`, else the core count (at
/// most 8).
pub fn thread_count(requested: usize) -> usize {
    if requested > 0 {
        return requested;
    }
    std::env::var("TS_R_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(4, |n| n.get())
                .min(8)
        })
        .max(1)
}

struct Queue<T> {
    items: VecDeque<T>,
    /// Items taken but not finished.
    active: usize,
}

/// Lets a task add more work.
pub struct Spawner<'a, T> {
    q: &'a Mutex<Queue<T>>,
    cv: &'a Condvar,
}

impl<T> std::fmt::Debug for Spawner<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Spawner")
    }
}

impl<T> Spawner<'_, T> {
    pub fn spawn(&self, item: T) {
        let mut q = self.q.lock().unwrap_or_else(|e| e.into_inner());
        q.items.push_back(item);
        drop(q);
        self.cv.notify_one();
    }
}

/// Runs `f` on every item, including items spawned by `f`, on `threads` workers. Returns
/// when all work is done.
pub fn run_dynamic<T, F>(threads: usize, items: Vec<T>, f: F)
where
    T: Send,
    F: Fn(T, &Spawner<'_, T>) + Sync,
{
    let q = Mutex::new(Queue {
        items: items.into_iter().collect(),
        active: 0,
    });
    let cv = Condvar::new();
    let worker = || {
        let sp = Spawner { q: &q, cv: &cv };
        loop {
            let item = {
                let mut g = q.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some(it) = g.items.pop_front() {
                        g.active += 1;
                        break Some(it);
                    }
                    if g.active == 0 {
                        break None;
                    }
                    g = cv.wait(g).unwrap_or_else(|e| e.into_inner());
                }
            };
            let Some(item) = item else {
                cv.notify_all();
                return;
            };
            f(item, &sp);
            let mut g = q.lock().unwrap_or_else(|e| e.into_inner());
            g.active -= 1;
            let done = g.active == 0 && g.items.is_empty();
            drop(g);
            if done {
                cv.notify_all();
            }
        }
    };
    let threads = threads.max(1);
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for _ in 0..threads {
            let h = std::thread::Builder::new()
                .stack_size(STACK)
                .spawn_scoped(s, worker);
            if let Ok(h) = h {
                handles.push(h);
            }
        }
        if handles.is_empty() {
            worker();
        }
        for h in handles {
            if let Err(e) = h.join() {
                std::panic::resume_unwind(e);
            }
        }
    });
}

/// Maps `items` in parallel, keeping order.
pub fn map<T, R, F>(threads: usize, items: Vec<T>, f: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync,
{
    let n = items.len();
    let out: Mutex<Vec<Option<R>>> = Mutex::new((0..n).map(|_| None).collect());
    let indexed: Vec<(usize, T)> = items.into_iter().enumerate().collect();
    run_dynamic(threads.min(n.max(1)), indexed, |(i, t), _| {
        let r = f(t);
        out.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
    });
    out.into_inner()
        .unwrap_or_else(|e| e.into_inner())
        .into_iter()
        .flatten()
        .collect()
}
