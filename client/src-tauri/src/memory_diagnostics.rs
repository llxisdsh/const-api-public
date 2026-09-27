//! Opt-in dev diagnostics. No allocation stacks, keys, prompts or response bodies.
//! Counts requested Rust allocation sizes, not allocator overhead, native C heaps,
//! reserved thread stacks or child WebViews. Debug-build totals are not a release baseline.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingAllocator;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static REALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

fn allocated(bytes: usize) {
    let current = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(current, Ordering::Relaxed);
}

// Forward the exact pointer, layout and alignment to the existing System allocator.
// Accounting itself never allocates or locks. Failed allocations change no live bytes.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, size) };
        if !result.is_null() {
            if size >= layout.size() {
                allocated(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
            REALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
}

#[cfg(target_os = "windows")]
fn process_memory() -> Option<serde_json::Value> {
    use windows_sys::Win32::System::{
        ProcessStatus::{
            K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        },
        Threading::GetCurrentProcess,
    };
    // GetCurrentProcess is a pseudo handle: it must not be closed.
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            counters.cb,
        )
    };
    (ok != 0).then(|| {
        serde_json::json!({
            "working_set_bytes": counters.WorkingSetSize,
            "peak_working_set_bytes": counters.PeakWorkingSetSize,
            "private_commit_bytes": counters.PrivateUsage,
        })
    })
}

#[cfg(not(target_os = "windows"))]
fn process_memory() -> Option<serde_json::Value> {
    None
}

pub(crate) fn snapshot() -> serde_json::Value {
    // Read before constructing the diagnostic JSON. Counters are approximate
    // concurrent snapshots; a realloc's internal temporary allocation is not visible.
    let live = LIVE.load(Ordering::Relaxed);
    let peak = PEAK.load(Ordering::Relaxed).max(live);
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let reallocations = REALLOCATIONS.load(Ordering::Relaxed);
    serde_json::json!({
        "schema_version": 1,
        "pid": std::process::id(),
        "observed_at_unix_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis(),
        "build": "debug",
        "rust_heap": {
            "live_bytes": live,
            "peak_live_bytes": peak,
            "allocations": allocations,
            "reallocations": reallocations,
        },
        "process": process_memory(),
        "continuation_cache": crate::supplier::codex_continuation_memory_diagnostics(),
        "activity": crate::update_activity::update_install_readiness(),
    })
}
