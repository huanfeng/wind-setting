//! 后台网络任务：把阻塞的 HTTP 下载挪出 UI 线程，结果经 channel 回来再跑续体。
//!
//! `ureq` 是阻塞 IO，放工作线程里正好，不引入异步运行时。难点只在「回到 UI 线程」：
//! 续体要写 Signal / 借 `AppState`，而它们都不是 `Send`，所以续体留在 UI 线程的
//! [`NetTasks::pending`] 表里，线程只把结果（`Send`）连同任务号发回；`main.rs` 注册的
//! channel 回调按号取出续体执行。
//!
//! 取消与迟到：[`NetTasks::cancel`] 摘掉续体并置取消标志——下载循环每个分块查一次标志，
//! 线程会很快收手；即便它已经下完，结果到达时找不到续体，直接丢弃，不会往已关的对话框
//! 里弹东西。

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub type TaskId = u64;

/// 工作线程发回 UI 线程的结果。载荷类型由 [`NetTasks::spawn`] 的调用方自己定。
pub struct NetDone {
    id: TaskId,
    payload: Box<dyn Any + Send>,
}

type Sink = Arc<dyn Fn(NetDone) + Send + Sync>;

/// 通道接上之前发起的任务：只记下「拿到发送端后怎么起线程」。
type Deferred = Box<dyn FnOnce(Sink)>;

struct Pending {
    cancel: Arc<AtomicBool>,
    done: Box<dyn FnOnce(Box<dyn Any + Send>)>,
}

#[derive(Default)]
pub struct NetTasks {
    sink: RefCell<Option<Sink>>,
    /// 冷启动的协议链接在 channel 注册之前就会路由，此时发起的任务排在这里等 `set_sink`。
    deferred: RefCell<Vec<Deferred>>,
    next_id: Cell<TaskId>,
    pending: RefCell<HashMap<TaskId, Pending>>,
}

impl NetTasks {
    /// 注入发送端。`main()` 在 App 构建期调用一次；此前排队的任务在这里启动。
    pub fn set_sink(&self, sink: impl Fn(NetDone) + Send + Sync + 'static) {
        let sink: Sink = Arc::new(sink);
        *self.sink.borrow_mut() = Some(sink.clone());
        for start in self.deferred.take() {
            start(sink.clone());
        }
    }

    /// 在后台线程跑 `work`，完成后在 UI 线程跑 `done`。通道没接上时先排队。
    ///
    /// `work` 拿到取消标志，应当在循环里查它。`done` 收到 `None` 表示工作线程 panic 了——
    /// 调用方必须在那条路上也收尾（复位忙标记等），否则界面会永远停在「下载中」。
    pub fn spawn<T: Send + 'static>(
        &self,
        work: impl FnOnce(&AtomicBool) -> T + Send + 'static,
        done: impl FnOnce(Option<T>) + 'static,
    ) -> TaskId {
        let id = self.next_id.get() + 1;
        self.next_id.set(id);
        let cancel = Arc::new(AtomicBool::new(false));
        self.pending.borrow_mut().insert(
            id,
            Pending {
                cancel: cancel.clone(),
                done: Box::new(move |payload| {
                    if let Ok(value) = payload.downcast::<Option<T>>() {
                        done(*value);
                    }
                }),
            },
        );
        let start: Deferred = Box::new(move |sink| {
            std::thread::spawn(move || {
                // 排队期间被取消的任务不必真去联网。
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let value =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&cancel))).ok();
                sink(NetDone {
                    id,
                    payload: Box::new(value),
                });
            });
        });
        let sink = self.sink.borrow().clone();
        match sink {
            Some(sink) => start(sink),
            None => self.deferred.borrow_mut().push(start),
        }
        id
    }

    /// 取消任务：摘掉续体（结果到了也不再处理）并通知工作线程收手。已完成的任务无副作用。
    pub fn cancel(&self, id: TaskId) {
        if let Some(p) = self.pending.borrow_mut().remove(&id) {
            p.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// channel 回调入口（UI 线程）。先摘出续体、放掉表的借用再执行：续体里可能再
    /// `spawn` / `cancel`。
    pub fn on_done(&self, msg: NetDone) {
        let pending = self.pending.borrow_mut().remove(&msg.id);
        if let Some(p) = pending {
            (p.done)(msg.payload);
        }
    }
}

/// 读取失败的三种原因，调用方各自配文案。
#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    TooLarge,
    Cancelled,
    Io(String),
}

/// 按分块读完 `r`，超过 `max` 字节立刻报错（不必读到尾），每块之间查一次取消标志。
pub fn read_limited(
    mut r: impl Read,
    max: usize,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, ReadError> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(ReadError::Cancelled);
        }
        let n = match r.read(&mut buf) {
            Ok(0) => return Ok(out),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ReadError::Io(e.to_string())),
        };
        if out.len() + n > max {
            return Err(ReadError::TooLarge);
        }
        out.extend_from_slice(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// 测试用的发送端：结果进队列，由测试代替 channel 回调去 `on_done`。
    fn tasks_with_queue() -> (NetTasks, Arc<Mutex<Vec<NetDone>>>) {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let q = queue.clone();
        let tasks = NetTasks::default();
        tasks.set_sink(move |m| q.lock().unwrap().push(m));
        (tasks, queue)
    }

    fn wait_for_one(queue: &Mutex<Vec<NetDone>>) -> NetDone {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(m) = queue.lock().unwrap().pop() {
                return m;
            }
            assert!(Instant::now() < deadline, "后台线程没有回结果");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn task_spawned_before_sink_starts_once_sink_is_set() {
        let tasks = NetTasks::default();
        let got = Rc::new(Cell::new(0));
        let g = got.clone();
        tasks.spawn(|_| 7, move |v: Option<i32>| g.set(v.unwrap()));

        let queue = Arc::new(Mutex::new(Vec::new()));
        let q = queue.clone();
        tasks.set_sink(move |m| q.lock().unwrap().push(m));
        tasks.on_done(wait_for_one(&queue));
        assert_eq!(got.get(), 7);
    }

    #[test]
    fn task_cancelled_while_queued_never_runs() {
        let tasks = NetTasks::default();
        let id = tasks.spawn(|_| panic!("排队期间已取消，不该联网"), |_: Option<i32>| {});
        tasks.cancel(id);
        let queue = Arc::new(Mutex::new(Vec::new()));
        let q = queue.clone();
        tasks.set_sink(move |m| q.lock().unwrap().push(m));
        std::thread::sleep(Duration::from_millis(50));
        assert!(queue.lock().unwrap().is_empty());
    }

    #[test]
    fn result_runs_continuation_on_the_calling_thread() {
        let (tasks, queue) = tasks_with_queue();
        let got = Rc::new(Cell::new(0));
        let g = got.clone();
        tasks.spawn(|_| 41 + 1, move |v: Option<i32>| g.set(v.unwrap()));
        assert_eq!(got.get(), 0, "续体要等 on_done 才跑");
        tasks.on_done(wait_for_one(&queue));
        assert_eq!(got.get(), 42);
    }

    #[test]
    fn cancelled_task_drops_late_result_and_flags_worker() {
        let (tasks, queue) = tasks_with_queue();
        let ran = Rc::new(Cell::new(false));
        let r = ran.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let id = tasks.spawn(
            move |cancel| {
                started_tx.send(()).unwrap();
                // 等取消信号，证明标志真的传到了工作线程。
                let deadline = Instant::now() + Duration::from_secs(5);
                while !cancel.load(Ordering::Relaxed) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                cancel.load(Ordering::Relaxed)
            },
            move |_: Option<bool>| r.set(true),
        );
        // 线程确实跑起来之后再取消（启动前取消的任务根本不会跑，另有测试）。
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        tasks.cancel(id);
        let msg = wait_for_one(&queue);
        assert!(
            msg.payload.downcast_ref::<Option<bool>>().unwrap().unwrap(),
            "工作线程应看到取消标志"
        );
        tasks.on_done(msg);
        assert!(!ran.get(), "取消后迟到的结果不得触发续体");
    }

    /// 工作线程 panic 也要回一条 `None`，否则调用方的「下载中」状态永远复位不了。
    #[test]
    fn worker_panic_is_reported_as_none() {
        let (tasks, queue) = tasks_with_queue();
        let got = Rc::new(Cell::new(None));
        let g = got.clone();
        tasks.spawn(
            |_| -> i32 { panic!("模拟下载线程崩溃") },
            move |v: Option<i32>| g.set(Some(v.is_none())),
        );
        tasks.on_done(wait_for_one(&queue));
        assert_eq!(got.get(), Some(true));
    }

    #[test]
    fn continuation_may_spawn_again() {
        let (tasks, queue) = tasks_with_queue();
        let tasks = Rc::new(tasks);
        let second = Rc::new(Cell::new(0));
        let (t, s) = (tasks.clone(), second.clone());
        tasks.spawn(
            |_| 1,
            move |_: Option<i32>| {
                let s = s.clone();
                t.spawn(|_| 2, move |v: Option<i32>| s.set(v.unwrap()));
            },
        );
        tasks.on_done(wait_for_one(&queue));
        tasks.on_done(wait_for_one(&queue));
        assert_eq!(second.get(), 2);
    }

    #[test]
    fn read_limited_reads_everything_under_the_limit() {
        let data = vec![7u8; 40_000];
        let out = read_limited(&data[..], 40_000, &AtomicBool::new(false)).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn read_limited_rejects_over_limit() {
        let data = vec![0u8; 40_001];
        assert_eq!(
            read_limited(&data[..], 40_000, &AtomicBool::new(false)),
            Err(ReadError::TooLarge)
        );
    }

    #[test]
    fn read_limited_stops_when_cancelled() {
        let data = [0u8; 100];
        assert_eq!(
            read_limited(&data[..], 1000, &AtomicBool::new(true)),
            Err(ReadError::Cancelled)
        );
    }

    #[test]
    fn read_limited_reports_io_errors() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("断了"))
            }
        }
        assert!(matches!(
            read_limited(Broken, 10, &AtomicBool::new(false)),
            Err(ReadError::Io(m)) if m.contains("断了")
        ));
    }
}
