//! `DCC_BIG_ALLOC_TRACE=<字节数>`：把 ≥ 阈值的分配连同调用栈打到 stderr。
//!
//! 为什么需要它：`fs/proc/kcore.c` 在 codegen 里申请一次 **4.5 GiB**，机器只有
//! 7.9 GB，构建期直接被 OOM 杀掉（`rc=137`）。`strace` 只能看到「一次 4.5 GiB 的
//! `mmap`」，看不出是谁申请的；release 二进制 `strip = "symbols"`，`perf` 也读不到
//! 符号。这里的追踪器挂在 `#[global_allocator]` 上，只在显式设了环境变量时工作
//! （默认只多一次 `OnceLock::get`），带符号构建 + `RUST_BACKTRACE=1` 就能拿到
//! 申请点。
//!
//! 纪律：**不参与任何判据、不改产物字节**，与 `DCC_DUMP_PASS`/`DCC_DCE_TRACE`
//! 同一档——报障用设施。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub struct BigAllocTrace;

/// 阈值；`0` = 关闭。
///
/// **为什么不在分配器里读 env**：`std::env::var` 自己要分配，而它在
/// `OnceLock::get_or_init` 的闭包里 —— 重入同一个 `OnceLock` 会**死锁**。
/// 实测：带追踪器编一个 5 行夹具直接卡到 timeout（连 `--version` 之外的任何
/// 编译都不返回）。所以阈值只在 [`init`] 里算一次，`init` 由 `main` 在启动时调，
/// 那时还不在分配器上下文里。`0` 是安全的默认值：没调过 `init` 的二进制
/// （单测/工具）就是关闭。
static LIMIT: AtomicUsize = AtomicUsize::new(0);

/// 读一次 `DCC_BIG_ALLOC_TRACE`（字节数；写非数字时取 256 MiB），之后固定。
pub fn init() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    let v = std::env::var("DCC_BIG_ALLOC_TRACE")
        .ok()
        .map(|s| s.trim().parse::<usize>().unwrap_or(256 * 1024 * 1024))
        .unwrap_or(0);
    LIMIT.store(v, Ordering::SeqCst);
}

fn note(size: usize) {
    let limit = LIMIT.load(Ordering::Relaxed);
    if limit == 0 || size < limit {
        return;
    }
    // `Backtrace` 自己会分配：BUSY 挡住递归。这里刻意不用 `format!`/`String`，
    // 少一次可能再次触发本函数的分配。
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::Relaxed) {
        return;
    }
    let bt = std::backtrace::Backtrace::force_capture();
    eprintln!("BIG-ALLOC size={size} bytes (threshold={limit})");
    eprintln!("{bt}");
    BUSY.store(false, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for BigAllocTrace {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note(l.size());
        System.alloc(l)
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        note(l.size());
        System.alloc_zeroed(l)
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        note(new);
        System.realloc(p, l, new)
    }
}
