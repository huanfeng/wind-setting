//! 内存诊断插桩（`--features memstat`，默认不编译）。
//!
//! 目的：把「进程占了多少」拆成「哪一段代码分配了多少」。任务管理器只给总量，
//! 而总量里既有控件树、也有图形栈与线程栈，光看总量无法判断该优化谁。
//!
//! 手段是包一层 [`std::alloc::System`] 计数：每次分配/释放各一条 relaxed 原子加，
//! 于是任意时刻的 `live` 就是本进程 Rust 侧的净堆占用。它不含图形驱动、GPU 显存、
//! 线程栈这些非 Rust 堆的部分——两者之差正好告诉我们「不是堆的那部分有多大」。
//!
//! 日志落在 `%TEMP%\wind_setting_memstat.log`：GUI 子系统没有控制台，stderr 会掉。

#[cfg(feature = "memstat")]
mod inner {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};

    static ALLOCATED: AtomicU64 = AtomicU64::new(0);
    static FREED: AtomicU64 = AtomicU64::new(0);
    static ALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);

    pub struct Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = unsafe { System.alloc(l) };
            if !p.is_null() {
                ALLOCATED.fetch_add(l.size() as u64, Relaxed);
                ALLOC_CALLS.fetch_add(1, Relaxed);
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            FREED.fetch_add(l.size() as u64, Relaxed);
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
            let q = unsafe { System.realloc(p, l, new) };
            if !q.is_null() {
                ALLOCATED.fetch_add(new as u64, Relaxed);
                FREED.fetch_add(l.size() as u64, Relaxed);
                ALLOC_CALLS.fetch_add(1, Relaxed);
            }
            q
        }
    }

    /// 当前净堆占用（字节）。
    pub fn live() -> u64 {
        ALLOCATED.load(Relaxed).saturating_sub(FREED.load(Relaxed))
    }

    /// 进程私有提交字节数（`PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`）。
    ///
    /// 计数分配器只看得见 Rust 堆；图形栈的 COM 对象、驱动缓冲、线程栈都在它之外。
    /// 两个数一起打，才分得清「这一步分配的是 Rust 数据结构」还是「这一步惊动了
    /// 别人家的分配器」——本次排查的关键差异正是后者。
    #[cfg(windows)]
    fn private_bytes() -> u64 {
        #[repr(C)]
        #[derive(Default)]
        struct Counters {
            cb: u32,
            page_fault_count: u32,
            peak_working_set_size: usize,
            working_set_size: usize,
            quota_peak_paged_pool: usize,
            quota_paged_pool: usize,
            quota_peak_non_paged_pool: usize,
            quota_non_paged_pool: usize,
            pagefile_usage: usize,
            peak_pagefile_usage: usize,
            private_usage: usize,
        }
        unsafe extern "system" {
            fn GetCurrentProcess() -> isize;
            fn K32GetProcessMemoryInfo(h: isize, c: *mut Counters, cb: u32) -> i32;
        }
        let mut c = Counters {
            cb: std::mem::size_of::<Counters>() as u32,
            ..Default::default()
        };
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
        if ok == 0 { 0 } else { c.private_usage as u64 }
    }

    #[cfg(not(windows))]
    fn private_bytes() -> u64 {
        0
    }

    fn log_path() -> std::path::PathBuf {
        std::env::temp_dir().join("wind_setting_memstat.log")
    }

    /// 打一个采样点：净堆、累计分配、分配次数，以及与上一采样点的净增量。
    pub fn mark(label: &str) {
        use std::io::Write;
        static LAST: AtomicU64 = AtomicU64::new(0);
        static LAST_PV: AtomicU64 = AtomicU64::new(0);
        let cur = live();
        let prev = LAST.swap(cur, Relaxed);
        let delta = cur as i64 - prev as i64;
        let pv = private_bytes();
        let pv_prev = LAST_PV.swap(pv, Relaxed);
        let pv_delta = pv as i64 - pv_prev as i64;
        let line = format!(
            "{:<26} 堆={:>8.2} ({:>+7.2}) MB   进程私有={:>8.2} ({:>+8.2}) MB   非堆={:>8.2} MB\n",
            label,
            cur as f64 / 1048576.0,
            delta as f64 / 1048576.0,
            pv as f64 / 1048576.0,
            pv_delta as f64 / 1048576.0,
            (pv.saturating_sub(cur)) as f64 / 1048576.0,
        );
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path())
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// 清空上一轮日志，写入表头。
    pub fn reset_log() {
        let _ = std::fs::write(log_path(), b"=== wind_setting memstat ===\n" as &[u8]);
    }
}

#[cfg(feature = "memstat")]
pub use inner::{Counting, mark, reset_log};

/// 未开 `memstat` 时的空实现：调用点无需 cfg，优化后不留指令。
#[cfg(not(feature = "memstat"))]
#[inline(always)]
pub fn mark(_label: &str) {}

#[cfg(not(feature = "memstat"))]
#[inline(always)]
pub fn reset_log() {}
