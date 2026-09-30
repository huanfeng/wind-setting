//! RPC 客户端：通过 JSON-RPC over named pipe 与核心服务通信。
//!
//! 复用 wind-ipc 的协议类型，保持单一来源。
//! 采用按调用连接模式（连接→发一帧→读一帧→关闭），适合设置操作的低频场景。

use serde_json::{Value, json};
use std::io::{Read, Write};

use wind_ipc::rpc::{PROTOCOL_VERSION, Request, Response};

const MAX_FRAME: u32 = 16 * 1024 * 1024;

/// 管道/socket 后缀。优先级：WIND_PIPE_SUFFIX 环境变量 > 运行时模式。
fn endpoint_suffix() -> String {
    std::env::var("WIND_PIPE_SUFFIX").unwrap_or_else(|_| crate::mode::pipe_suffix().to_string())
}

/// 控制通道端点路径。与 wind-rpc 的 ctrl_endpoint 一致。
pub fn ctrl_endpoint() -> String {
    let suffix = endpoint_suffix();
    #[cfg(windows)]
    {
        // 末尾化，对齐 core wind-rpc::server::ctrl_endpoint
        // （release: wind_input_ctrl，dev: wind_input_ctrl_dev）。
        format!(r"\\.\pipe\wind_input_ctrl{suffix}")
    }
    #[cfg(unix)]
    {
        // unix 侧 core 用中间化 wind_input{suffix}_ctrl.sock，保持一致。
        let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        format!("{dir}/wind_input{suffix}_ctrl.sock")
    }
}

/// 字节数的人读形态。上限类报错要让用户一眼看出差多少，`18874368 > 16777216` 做不到。
fn human_bytes(n: u64) -> String {
    const MB: f64 = (1024 * 1024) as f64;
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / MB)
    } else {
        format!("{n} 字节")
    }
}

/// 写一帧（4 字节大端长度 + JSON payload）。
///
/// ## 发送前就要拦，而不是发出去让对面读不下
///
/// 上限是双向的：这一侧 [`read_frame`] 认 [`MAX_FRAME`]，core 侧
/// `wind_ipc::rpc::MAX_MESSAGE_SIZE` 同值。但 core 的写侧**不检查长度**，会老实把超大
/// 响应推出来；从前这一侧的写也不检查。于是「自己发的东西太大」这件事，只能等对面
/// 读长度前缀时才发现，报出来的是**「读取失败: 帧长超限」——方向是反的**，用户与其说
/// 看到一条诊断，不如说看到一条误导（issue #101 那位用户看到的正是这句）。
///
/// 在这里拦，错误就落在「发送」上、并带上实际大小与上限，谁太大一目了然。这对**所有**
/// 走 `content` 的方法一次性生效，不必逐个方法去改传输方式。
///
/// ⚠️ 顺带封死一个静默截断：原先 `payload.len() as u32` 在 payload 超 4 GiB 时会**截断**
/// 成一个小数字，长度前缀与实际内容对不上，对面读出的是半截垃圾而非报错。检查放在转换
/// 之前，那条路就不存在了。
fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> std::io::Result<()> {
    if payload.len() > MAX_FRAME as usize {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "请求体 {} 超过单帧上限 {}（数据量太大，请分批或改用文件导入导出）",
                human_bytes(payload.len() as u64),
                human_bytes(MAX_FRAME as u64),
            ),
        ));
    }
    let len = payload.len() as u32;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// 读一帧。
fn read_frame<R: Read>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME {
        // 说清是**对面回来的**东西太大，且报出大小——与 `write_frame` 那条合起来，
        // 两个方向的超限从此各说各的，不会再互相冒充。
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "core 返回 {} 超过单帧上限 {}（结果太大，请改用文件导出或缩小范围）",
                human_bytes(len as u64),
                human_bytes(MAX_FRAME as u64),
            ),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// 在一条已连接的流上完成一次请求-响应。
fn call_on_stream<S: Read + Write>(
    stream: &mut S,
    method: &str,
    params: &Value,
) -> Result<Value, String> {
    let req = Request {
        version: PROTOCOL_VERSION,
        id: 1,
        method: method.to_string(),
        params: params.clone(),
    };
    let payload = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
    write_frame(stream, &payload).map_err(|e| format!("发送失败: {e}"))?;
    let resp_buf = read_frame(stream).map_err(|e| format!("读取失败: {e}"))?;
    let resp: Response = serde_json::from_slice(&resp_buf).map_err(|e| e.to_string())?;
    if let Some(err) = resp.error {
        return Err(err);
    }
    Ok(resp.result.unwrap_or(Value::Null))
}

/// 单次 RPC 的整体超时上限。命名管道 read_exact 本身无超时,core 无响应时会无限
/// 阻塞;该上限把最坏等待收敛到有限值,避免拖住 UI 线程与进程退出(见下方线程包装)。
///
/// ⚠️ 这只管**交互类**调用。批量方法走 [`BULK_TIMEOUT`],判据见 [`is_bulk_method`]。
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 批量方法的超时上限。
///
/// ## 为什么不能让它们共用 5 秒
///
/// 5 秒是按「设置页点一下、core 查个配置回来」定的,导入导出/备份还原却是 **O(词库条数)**
/// 的活:几十万条写 redb 远不止 5 秒。超时后客户端只是**放弃等待**,core 那边的工作线程
/// **照样跑到完成** —— 于是用户看到「core 无响应」以为失败了,重试一次,数据被导入两遍。
/// 「报失败其实成功」是这里最坏的形态,比单纯的慢危险得多。
///
/// 取 10 分钟:够覆盖百万级词库,又不至于在 core 真的死掉时把设置页永久挂住
/// (那种情况下用户还有关窗口这条路,而窗口关得掉正是因为 RPC 跑在工作线程上)。
const BULK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// 该方法是否属于「工作量随词库规模增长」的批量方法。
///
/// ★ 判据是**方法名前缀/整名白名单**,不是「参数里有没有大字符串」——后者会把
/// 一次误传的大 payload 也放行成长任务,而前者是按语义划的,读代码就能核对。
///
/// 只列真正 O(n) 的:导入导出要遍历全库,备份还原要打包整个用户目录,清空与批量转正
/// 要逐条改 redb。`*.list`/`*.listPaged` 等**有意不在内**:它们已经分页,若某天变慢了,
/// 该修的是分页而不是把超时调大。
fn is_bulk_method(method: &str) -> bool {
    matches!(
        method,
        "dict.export"
            | "dict.import"
            | "dict.previewImport"
            | "dict.clear"
            | "dict.encodeWords"
            | "backup.create"
            | "backup.inspect"
            | "backup.restore"
            | "phrase.import"
            | "phrase.export"
            | "quick.import"
            | "quick.export"
            | "commonChars.import"
            | "commonChars.export"
            | "scheme.exportPackage"
            | "scheme.importPackage"
            | "temp.promoteAll"
            | "temp.clear"
            | "freq.clear"
            // 补全学习数据的清空同样是 O(n) 改 redb：网址历史条数上限出厂 200，但用户
            // 可以调到上万。不进这张表就走 5 秒档，数据量大时会「报失败其实成功」。
            | "mailSuffix.clear"
            | "urlHistory.clear"
    )
}

/// 按方法取超时档。
fn timeout_for(method: &str) -> std::time::Duration {
    if is_bulk_method(method) {
        BULK_TIMEOUT
    } else {
        CALL_TIMEOUT
    }
}

/// 测试用互斥锁：`WIND_RPC_MOCK` 是**进程全局**环境变量,任何读写它的测试都必须先持有本锁。
///
/// 必须全 crate 共用一把。曾经 `state` 与 `theme_import` 各自定义了一个同名 static,
/// 两把锁互不相识却保护同一个变量 —— 一侧刚 `set_var`,另一侧测试结束时的 `remove_var`
/// 就把它抹掉,导致 `mock_disconnect_does_not_block` 随并行调度顺序间歇失败。
/// 新增涉及该变量的测试时,一律 `let _g = crate::rpc::ENV_LOCK.lock()...`,不要另建锁。
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 连接核心并完成一次 RPC 调用。
pub fn call_core(method: &str, params: &Value) -> Result<Value, String> {
    if std::env::var("WIND_RPC_MOCK").as_deref() == Ok("1") {
        return mock_result(method, params);
    }
    let endpoint = ctrl_endpoint();
    let t = std::time::Instant::now();
    // 在工作线程执行阻塞式管道 RPC,主调用侧带超时等待:命名管道读取无超时,core
    // 无响应会无限阻塞。超时后放弃该线程(其阻塞的 read 会随进程退出由 OS 回收),
    // 立即返回错误,从而 UI 不卡、退出不被拖住。设置操作低频,每次 spawn 开销可忽略。
    let (tx, rx) = std::sync::mpsc::channel();
    let method_owned = method.to_string();
    let params_owned = params.clone();
    std::thread::spawn(move || {
        let _ = tx.send(call_core_inner(&endpoint, &method_owned, &params_owned));
    });
    let timeout = timeout_for(method);
    let result = match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => Err(format!("core 无响应(超过 {}s)", timeout.as_secs())),
    };
    match &result {
        Ok(_) => tracing::debug!("rpc {} ok ({:?})", method, t.elapsed()),
        Err(e) => tracing::warn!("rpc {} err ({:?}): {}", method, t.elapsed(), e),
    }
    result
}

fn call_core_inner(endpoint: &str, method: &str, params: &Value) -> Result<Value, String> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(endpoint)
            .map_err(|e| format!("连接 core 失败 ({endpoint}): {e}"))?;
        call_on_stream(&mut stream, method, params)
    }
    #[cfg(windows)]
    {
        let mut stream =
            connect_pipe(endpoint, 100).map_err(|e| format!("连接 core 失败 ({endpoint}): {e}"))?;
        call_on_stream(&mut stream, method, params)
    }
}

/// Windows 命名管道连接，对瞬态错误短重试。
#[cfg(windows)]
fn connect_pipe(endpoint: &str, max_attempts: u32) -> std::io::Result<std::fs::File> {
    let mut attempt = 0u32;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(endpoint)
        {
            Ok(s) => return Ok(s),
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                // 231 = ERROR_PIPE_BUSY：服务存在但繁忙，重试有意义。
                // 2/3 = 管道/路径不存在：服务未启动，立即失败，勿重试浪费 2s。
                if code == 231 && attempt < max_attempts {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                } else {
                    return Err(e);
                }
            }
        }
    }
}

/// 是否处于离线 mock 模式。
pub fn is_mock() -> bool {
    std::env::var("WIND_RPC_MOCK").as_deref() == Ok("1")
}

// ─────────────────── 事件订阅（core → 设置端单向推送） ───────────────────
//
// 控制通道是请求-响应的，只能"问"，问不到 core 自发的状态变更（用户用快捷键/托盘
// 切了方案等）。core 另起了一条单向事件管道，这里订阅它，让界面能跟着变。

/// 断线后的重连间隔。core 未启动时会一直按此节奏重试，单次成本仅一次 open 失败。
const EVENTS_RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

/// 事件通道端点。**命名规则与 [`ctrl_endpoint`] 不一致**：core 侧 ctrl 是末尾化
/// （`wind_input_ctrl{suffix}`），events 是中间化（`wind_input{suffix}_events`）——
/// 见 wind-rpc/src/server.rs 的 `ctrl_endpoint` / `events_endpoint`。
///
/// 这里手抄格式而非直接调 core 的函数：wind-rpc 只是 dev-dependency（运行时依赖
/// 刻意只保留轻量的 wind-ipc，见 Cargo.toml）。抄错的后果是静默连不上——事件永远
/// 收不到，而不是报错，故由 [`tests::events_endpoint_matches_core`] 对账兜住。
pub fn events_endpoint() -> String {
    let suffix = endpoint_suffix();
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\wind_input{suffix}_events")
    }
    #[cfg(unix)]
    {
        let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        format!("{dir}/wind_input{suffix}_events.sock")
    }
}

/// core 广播的一条事件（线路格式 `{event, data}`，帧编码与控制通道同为
/// 4 字节大端长度 + JSON，故可复用 [`read_frame`]）。
#[derive(Debug, Clone)]
pub struct CoreEvent {
    pub event: String,
    pub data: Value,
}

/// 打开事件流。Windows 侧 core 用的是单向管道（`PIPE_ACCESS_OUTBOUND`），
/// **必须只读打开**——带上 write 权限会被拒（不同于双向的 ctrl 通道）。
#[cfg(windows)]
fn open_events_stream(endpoint: &str) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().read(true).open(endpoint)
}

#[cfg(unix)]
fn open_events_stream(endpoint: &str) -> std::io::Result<std::os::unix::net::UnixStream> {
    std::os::unix::net::UnixStream::connect(endpoint)
}

/// 解析一帧事件载荷（已剥去长度前缀的 JSON）。`event` 缺失即视为不可解析。
/// 抽成纯函数供 [`tests::parse_event_frame_matches_core_encoding`] 与 core 的
/// `EventMessage` 序列化对账。
fn parse_event_frame(payload: &[u8]) -> Option<CoreEvent> {
    let v: Value = serde_json::from_slice(payload).ok()?;
    let event = v.get("event")?.as_str()?.to_string();
    Some(CoreEvent {
        event,
        data: v.get("data").cloned().unwrap_or(Value::Null),
    })
}

/// 阻塞订阅 core 事件，每收到一条调一次 `on_event`；断线自动重连，**永不返回**。
/// 调用方应在专用后台线程里跑它。mock 模式无 core 可连，直接返回。
pub fn listen_events(mut on_event: impl FnMut(CoreEvent)) {
    if is_mock() {
        return;
    }
    let endpoint = events_endpoint();
    loop {
        match open_events_stream(&endpoint) {
            Ok(mut stream) => {
                tracing::info!("已订阅 core 事件通道 {}", endpoint);
                // 读出错即 core 退出或管道关闭，跳出由外层重连。
                while let Ok(frame) = read_frame(&mut stream) {
                    match parse_event_frame(&frame) {
                        Some(ev) => on_event(ev),
                        None => tracing::warn!("事件帧无法解析，已跳过"),
                    }
                }
                tracing::info!("core 事件通道已断开，{EVENTS_RECONNECT_DELAY:?} 后重连");
            }
            // core 未启动是常态（设置界面可独立打开），故只记 debug。
            Err(e) => tracing::debug!("事件通道连接失败({e})，稍后重试"),
        }
        std::thread::sleep(EVENTS_RECONNECT_DELAY);
    }
}

// 测试模块后仍有 mock_reply 等离线实现；局部豁免 toolchain 新增的风格 lint。
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;

    /// ★★ 超限的请求要在**写出任何字节之前**被拒。
    ///
    /// 若先写长度前缀再发现超限，管道里就留下 4 字节垃圾，后续帧全部错位——那是比
    /// 「数据太大」难查得多的一类故障。断言 sink 为空，锁的就是这个顺序。
    ///
    /// 另一半是**报错方向**：从前这一侧的写不检查，超大请求发出去、由对面读长度前缀时
    /// 才拒，用户看到的是「读取失败: 帧长超限」——自己发的东西太大，报得像对方的问题
    /// （issue #101 那位用户看到的正是这句）。
    #[test]
    fn oversized_request_is_rejected_before_writing_anything() {
        let mut sink: Vec<u8> = Vec::new();
        let err = write_frame(&mut sink, &vec![b'x'; 20 * 1024 * 1024]).unwrap_err();
        assert!(sink.is_empty(), "★ 拒绝必须发生在写出任何字节之前");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        let msg = err.to_string();
        assert!(msg.contains("请求体"), "要说清是发送方向：{msg}");
        assert!(
            msg.contains("20.0 MB") && msg.contains("16.0 MB"),
            "要报出实际大小与上限：{msg}"
        );

        // 边界：恰好等于上限放行，+1 拒绝
        let mut sink: Vec<u8> = Vec::new();
        assert!(write_frame(&mut sink, &vec![0u8; MAX_FRAME as usize]).is_ok());
        assert_eq!(sink.len(), 4 + MAX_FRAME as usize);
        assert!(write_frame(&mut Vec::new(), &vec![0u8; MAX_FRAME as usize + 1]).is_err());
    }

    /// 两个方向的超限各说各的，不互相冒充。
    ///
    /// 读侧只需 4 字节长度前缀就能判——**不必真造一个超大响应**，也正因如此，从前那句
    /// 「帧长超限」看不出说的是哪一头。
    #[test]
    fn oversized_response_reports_the_other_direction() {
        let mut frame = (MAX_FRAME + 1).to_be_bytes().to_vec();
        frame.extend_from_slice("（后面的内容根本读不到）".as_bytes());
        let err = read_frame(&mut std::io::Cursor::new(frame)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("core 返回"), "要说清是接收方向：{msg}");
        assert!(
            !msg.contains("请求体"),
            "★ 不能与发送方向的措辞混用，否则又回到分不清谁太大的老样子：{msg}"
        );

        // 正常帧照常读回
        let body = b"{\"ok\":true}";
        let mut ok = (body.len() as u32).to_be_bytes().to_vec();
        ok.extend_from_slice(body);
        assert_eq!(
            read_frame(&mut std::io::Cursor::new(ok)).unwrap(),
            body.to_vec()
        );
    }

    /// 人读字节数：上限类报错要让人一眼看出差多少。
    #[test]
    fn human_bytes_switches_unit_at_one_mb() {
        assert_eq!(human_bytes(0), "0 字节");
        assert_eq!(human_bytes(999), "999 字节");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(human_bytes(16 * 1024 * 1024), "16.0 MB");
    }

    // 测试遮蔽：旧用例只传 method；转调真实双参实现。
    fn mock_reply(method: &str) -> Value {
        super::mock_reply(method, &json!({}))
    }

    /// 带 params 的 mock 取值（部分应答按请求内容分形，见 `mock_wants_long_info`）。
    fn mock_reply_with(method: &str, params: &Value) -> Value {
        super::mock_reply(method, params)
    }

    #[test]
    fn test_ctrl_endpoint_format() {
        let ep = ctrl_endpoint();
        assert!(ep.contains("wind_input"));
        assert!(ep.contains("_ctrl"));
    }

    /// 守护：事件帧的解析必须与 core 实际发出的 `EventMessage` 序列化结果对得上。
    /// 用 core 自己的类型构造输入（wind-ipc 是硬依赖），字段改名会在此处失败，
    /// 而不是等到运行时表现为"事件收得到但内容全空"。
    #[test]
    fn parse_event_frame_matches_core_encoding() {
        let msg = wind_ipc::rpc::EventMessage {
            event: "schema.activeChanged".to_string(),
            data: json!({ "id": "wubi86" }),
        };
        let payload = serde_json::to_vec(&msg).expect("EventMessage 可序列化");
        let ev = super::parse_event_frame(&payload).expect("应能解析 core 的事件帧");
        assert_eq!(ev.event, "schema.activeChanged");
        assert_eq!(ev.data.get("id").and_then(|v| v.as_str()), Some("wubi86"));

        // 非 JSON / 缺 event 字段：跳过而不是 panic。
        assert!(super::parse_event_frame(b"not json").is_none());
        assert!(super::parse_event_frame(br#"{"data":{}}"#).is_none());
    }

    /// 守护：事件端点必须与 core 的 `wind_rpc::events_endpoint` 逐字一致。
    ///
    /// 抄错不会报错，只会静默收不到任何事件——「当前方案」之类的显示就悄悄退回
    /// 打开那一刻的快照，且看起来一切正常。这种失败模式必须由测试兜住。
    /// 主代码不能直接调该函数（wind-rpc 是 dev-dependency），故用对账代替复用。
    #[test]
    fn events_endpoint_matches_core() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 覆盖 release（空后缀）与 dev 两种变体。
        for suffix in ["", "_dev"] {
            unsafe { std::env::set_var("WIND_PIPE_SUFFIX", suffix) };
            assert_eq!(
                super::events_endpoint(),
                wind_rpc::events_endpoint(suffix),
                "事件端点与 core 漂移(suffix={suffix:?})"
            );
        }
        unsafe { std::env::remove_var("WIND_PIPE_SUFFIX") };
    }

    /// mockdata 中刻意偏离 core 系统预置的值（配套 [`mock_config_matches_core_system_preset`]）。
    ///
    /// `schema.available` 必须与 mock 的 `schema.list` 自洽（那里给的是 wubi86 / wubi98 /
    /// wubi_mixed / pinyin_simp 这套演示方案），不能取真实预置的 wubi86_pinyin——
    /// 否则 mock 模式的方案下拉会列出一个 `schema.list` 里不存在的 id。
    const CURATED_MOCK_VALUES: &[&str] = &["schema.available"];

    /// 把 JSON 摊平成「点分路径 → 叶子值」（规则同 core 的 collect_leaf_keys：
    /// 非空对象下钻，数组与空对象作叶子）。
    fn json_leaves(prefix: &str, v: &Value, out: &mut std::collections::BTreeMap<String, Value>) {
        match v {
            Value::Object(m) if !m.is_empty() => {
                for (k, sub) in m {
                    let child = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    json_leaves(&child, sub, out);
                }
            }
            _ => {
                out.insert(prefix.to_string(), v.clone());
            }
        }
    }

    /// 守护：`mockdata/config.json` 必须等于 core 的系统预置（L1 代码默认 ⊕ data/config.toml），
    /// 即全新安装下 `config.get` 的真实返回值；仅 [`CURATED_MOCK_VALUES`] 中的键可以不同。
    ///
    /// mockdata 是 core `Config` 的**手抄镜像**，此前无任何对账，已烂到：缺 45 键、
    /// 残留 6 个 core 已删除的孤儿键（`input.code_commit.*` 在 6 域重命名时就迁走了、
    /// `schema.pinyin.candidate_order` 已下线），另有 7 处值停留在 L1 而非成品的 L1⊕L2。
    /// 后果是 mock 模式下的设置页与真实运行时不是同一个东西，拿它验 UI 会得出错误结论。
    ///
    /// 失败时请重新生成 mockdata，而不是逐个手改。
    #[test]
    fn mock_config_matches_core_system_preset() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../WindInput/data");
        let preset =
            wind_config::Config::system_preset_value(Some(&data)).expect("读取 core 系统预置失败");
        let cfg: wind_config::Config = preset.try_into().expect("系统预置反序列化为 Config 失败");
        let expect = serde_json::to_value(&cfg).expect("Config 序列化失败");
        let mock: Value = serde_json::from_str(MOCK_CONFIG).expect("mockdata JSON 解析失败");

        let (mut e, mut m) = (Default::default(), Default::default());
        json_leaves("", &expect, &mut e);
        json_leaves("", &mock, &mut m);
        assert!(!e.is_empty(), "core 预置为空，对账无意义");

        let mut problems = Vec::new();
        for (key, want) in &e {
            match m.get(key) {
                None => problems.push(format!("mockdata 缺键: {key}")),
                Some(got) if got != want && !CURATED_MOCK_VALUES.contains(&key.as_str()) => {
                    problems.push(format!("{key}: mock={got} core={want}"));
                }
                _ => {}
            }
        }
        for key in m.keys().filter(|k| !e.contains_key(*k)) {
            problems.push(format!("mockdata 多余键（core 已无此键）: {key}"));
        }
        assert!(
            problems.is_empty(),
            "mockdata/config.json 与 core 系统预置漂移 {} 处，请重新生成：\n\
             cargo test regenerate_mock_config -- --ignored\n{}",
            problems.len(),
            problems.join("\n")
        );
    }

    /// 重新生成 `mockdata/config.json`（**非自动运行**，需显式 `-- --ignored`）。
    ///
    /// 上面的守护测试要求「重新生成而不是逐个手改」，此前却没有生成入口，只能照着
    /// 报错逐条补——正是它要防的手抄漂移。core 改了配置默认值后跑：
    /// `cargo test regenerate_mock_config -- --ignored`
    ///
    /// [`CURATED_MOCK_VALUES`] 中的键沿用旧 mock 的值：它们**有意**偏离 core 预置，
    /// 直接覆盖会把那份策展改掉（`curated_mock_values_still_differ` 会当场发现）。
    ///
    /// 缩进用 1 空格而非 `to_string_pretty` 的 2 空格，与检入格式一致——否则一次
    /// 再生成会把 300 余行全部重排，真正的改动淹没在噪声里。
    #[test]
    #[ignore = "写文件的维护工具，按需显式运行"]
    fn regenerate_mock_config() {
        use serde::Serialize;

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let preset =
            wind_config::Config::system_preset_value(Some(&root.join("../WindInput/data")))
                .expect("读取 core 系统预置失败");
        let cfg: wind_config::Config = preset.try_into().expect("系统预置反序列化为 Config 失败");
        let mut fresh = serde_json::to_value(&cfg).expect("Config 序列化失败");

        let old: Value = serde_json::from_str(MOCK_CONFIG).expect("旧 mockdata 解析失败");
        for key in CURATED_MOCK_VALUES {
            let ptr = format!("/{}", key.replace('.', "/"));
            let want = old.pointer(&ptr).expect("策展键不在旧 mockdata 中").clone();
            *fresh.pointer_mut(&ptr).expect("策展键不在 core 配置中") = want;
        }

        let mut buf = Vec::new();
        let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
        fresh.serialize(&mut ser).expect("序列化失败");
        buf.push(b'\n'); // 与检入格式一致（EOF 换行）

        let path = root.join("src/mockdata/config.json");
        std::fs::write(&path, buf).expect("写入 mockdata 失败");
        println!("已重新生成 {}", path.display());
    }

    /// 守护：策展名单不能腐烂——列进去的键必须真的与预置不同，否则说明它已可回归自动对账。
    #[test]
    fn curated_mock_values_still_differ() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../WindInput/data");
        let preset = wind_config::Config::system_preset_value(Some(&data)).expect("读取预置失败");
        let cfg: wind_config::Config = preset.try_into().expect("反序列化失败");
        let expect = serde_json::to_value(&cfg).expect("序列化失败");
        let mock: Value = serde_json::from_str(MOCK_CONFIG).expect("解析失败");

        let (mut e, mut m) = (Default::default(), Default::default());
        json_leaves("", &expect, &mut e);
        json_leaves("", &mock, &mut m);
        for key in CURATED_MOCK_VALUES {
            assert!(e.contains_key(*key), "策展键已不在 core 配置中: {key}");
            assert_ne!(
                m.get(*key),
                e.get(*key),
                "{key} 现在与 core 预置相同，请从 CURATED_MOCK_VALUES 移除"
            );
        }
    }

    /// mock 的 `config.getDefaults` 必须给出可用的 Config（此前返回空对象，
    /// 导致 mock 模式下「恢复默认」静默无效）。
    #[test]
    fn mock_get_defaults_returns_config_shape() {
        let d = mock_reply("config.getDefaults");
        assert!(d.is_object(), "getDefaults 应返回对象");
        assert!(
            d.get("schema").and_then(|s| s.get("active")).is_some(),
            "getDefaults 应是 Config 形状（含 schema.active），实际: {d}"
        );
    }

    /// 模板预览的 mock 与真实 core 同形（字段齐全、problems 用模板偏移、气泡无选中态），
    /// 离线截图里预览行与「插入颜色」才走得通。
    #[test]
    fn mock_preview_template_has_core_shape() {
        let r = mock_reply_with(
            "appearance.previewTemplate",
            &json!({"template": "$[nope]{x} ${code}", "scene": "comment", "swatches": ["error"]}),
        );
        for k in [
            "text", "fg", "bg", "runs", "selected", "problems", "swatches",
        ] {
            assert!(r.get(k).is_some(), "缺 {k}：{r}");
        }
        assert_eq!(r["swatches"]["error"], json!([217, 48, 37, 255]));
        let p = &r["problems"][0];
        assert_eq!(
            (p["start"].as_u64(), p["end"].as_u64()),
            (Some(2), Some(6)),
            "模板偏移"
        );
        let t = mock_reply_with(
            "appearance.previewTemplate",
            &json!({"template": "x", "scene": "content", "swatches": ["error"]}),
        );
        assert_eq!(t["selected"], Value::Null, "气泡没有选中态");
        assert_eq!(
            t["swatches"]["error"],
            json!([242, 139, 130, 255]),
            "气泡取 tooltip_ 色"
        );
    }

    /// 守护：统计样例必须能被页面的数据层按 core 契约解析。
    ///
    /// mock 与 `data::DailyItem`/`Summary` 的字段名（core 的压缩缩写）一旦对不上，
    /// 表现是「离线模式统计页全零」——静默且容易误判成"功能没做"，故在此锁死。
    #[test]
    fn mock_stats_parses_as_page_data() {
        use crate::pages::stats::data::{DailyItem, Summary};

        let daily = super::mock_reply(
            "stats.daily",
            &json!({ "from": "2026-01-01", "to": "2026-01-31" }),
        );
        let days: Vec<DailyItem> = serde_json::from_value(daily).expect("daily 应能解析");
        assert_eq!(days.len(), 31, "区间内每天都要有一条（含零值日）");
        assert!(days.iter().any(|d| d.total > 0), "样例不应全是空白日");
        assert!(
            days.iter().any(|d| d.total == 0),
            "样例应含空白日，否则热力图色阶 0 档无从核对"
        );
        // 时段分布必须与当日总量自洽，否则「占今日 x%」会算出 >100%。
        for d in days.iter().filter(|d| d.total > 0) {
            assert_eq!(
                d.hours.iter().sum::<u32>(),
                d.total,
                "{} 的时段分布之和应等于总字数",
                d.date
            );
            assert!(!d.by_schema.is_empty(), "{} 应有方案分项", d.date);
        }

        let summary: Summary =
            serde_json::from_value(super::mock_reply("stats.summary", &json!({})))
                .expect("summary 应能解析");
        assert!(summary.total_chars > 0);
        assert!(summary.active_days > 0);
        assert!(
            summary.avg_code_len > 0.0 && summary.first_select_rate > 0.0,
            "码长/首选率应非零，否则详情区整块隐藏"
        );
    }

    #[test]
    fn test_mock_reply_shapes() {
        // 确保 mock 返回合理的 JSON 结构
        let status = mock_reply("system.status");
        assert!(status.get("running").is_some());

        let config = mock_reply("config.get");
        assert!(config.is_object());

        let list = mock_reply("schema.list");
        assert!(list.is_array());

        let paged = mock_reply("dict.listPaged");
        assert!(paged.get("items").is_some());
        assert!(paged.get("total").is_some());
    }

    #[test]
    fn mock_phrase_list_has_is_system() {
        let list = mock_reply("phrase.list");
        let arr = list.as_array().unwrap();
        assert!(arr.iter().any(|p| p.get("isSystem") == Some(&json!(true))));
        assert!(arr.iter().any(|p| p.get("isSystem") == Some(&json!(false))));
    }

    /// mock 的配置片段导入形状须与 core 契约一致：previewPatch 每条含 key/next
    /// （current 可缺席=未设置，合法条目无 error）；applyPatch 含 ok/applied/needsRestart。
    #[test]
    fn mock_config_patch_shapes() {
        let p = mock_reply("config.previewPatch");
        assert_eq!(p.get("ok"), Some(&json!(true)));
        let entries = p
            .get("entries")
            .and_then(|v| v.as_array())
            .expect("entries 数组");
        assert!(!entries.is_empty());
        assert!(
            entries
                .iter()
                .all(|e| e.get("key").is_some() && e.get("next").is_some())
        );
        assert!(
            entries.iter().all(|e| e.get("error").is_none()),
            "合法条目不带 error"
        );

        // Map 键的条目行：key = 父键、mapEntry = 条目名（条目名不并进点分键）。
        let map_row = entries
            .iter()
            .find(|e| e.get("key") == Some(&json!("keys.key_actions")))
            .expect("样例应含一条 Map 条目");
        assert_eq!(map_row.get("mapEntry"), Some(&json!("backtick")));

        let a = mock_reply("config.applyPatch");
        assert_eq!(a.get("ok"), Some(&json!(true)));
        assert!(a.get("applied").and_then(|v| v.as_u64()).is_some());
        assert!(a.get("needsRestart").and_then(|v| v.as_bool()).is_some());
        // written = 落盘后的最终键值；Map 父键带合并后的整表（不止片段里那一条）。
        let written = a
            .get("written")
            .and_then(|v| v.as_array())
            .expect("written 数组");
        assert!(
            written
                .iter()
                .all(|w| w.get("key").is_some() && w.get("value").is_some())
        );
        let map_written = written
            .iter()
            .find(|w| w.get("key") == Some(&json!("keys.key_actions")))
            .and_then(|w| w.get("value"))
            .and_then(|v| v.as_object())
            .expect("Map 父键的 written 值应是整表");
        assert!(
            map_written.len() > 1,
            "Map 父键必须带合并后的整表（含片段未提及的既有条目），实际: {map_written:?}"
        );
    }

    /// 说明元信息（§2.3）的 mock 形状，三件事：
    ///
    /// ① **有说明 / 无说明两形都在**——信封样例带说明，纯方案包（`scheme.previewImport`）
    ///    不带，后者是「区块整块不占位」那条路离线唯一能走到的样例；
    /// ② 同一个信封响应里，**包级说明与 `configPatch.info` 取值不同**——写成同源的话，
    ///    UI 把两级合并、或某一处串了源，离线怎么点都看不出来；
    /// ③ 文本带 `mock_long_info` 记号时说明撑到 core 限额上限（title 200 /
    ///    description 4000 字符），供显示封顶的离线核对。样例说明都是两三行的短文本，
    ///    光靠它们测不出封顶有没有生效。
    #[test]
    fn mock_package_info_shapes() {
        let s = |v: &Value, p: &str| {
            v.pointer(p)
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        };

        // 片段：previewPatch 带 info。
        let p = mock_reply("config.previewPatch");
        assert!(!s(&p, "/info/title").is_empty());
        assert!(!s(&p, "/info/description").is_empty());

        // 纯方案包：无说明形（顶层两字段都缺席）。
        let zip = mock_reply("scheme.previewImport");
        assert!(zip.get("title").is_none() && zip.get("description").is_none());

        // 信封：两级都在，且四个值互不相同。
        let env = super::mock_result(
            "scheme.previewImportText",
            &json!({ "text": "kind = \"schema_text\"" }),
        )
        .expect("信封应预览成功");
        let vals = [
            s(&env, "/title"),
            s(&env, "/description"),
            s(&env, "/configPatch/info/title"),
            s(&env, "/configPatch/info/description"),
        ];
        assert!(
            vals.iter().all(|v| !v.is_empty()),
            "两级说明都要给: {vals:?}"
        );
        for (i, a) in vals.iter().enumerate() {
            for b in &vals[i + 1..] {
                assert_ne!(a, b, "两级说明取值必须互不相同，否则测不出串源");
            }
        }

        // 限额上限形：记号在片段文本里，两法都要认。
        let long = mock_reply_with(
            "config.previewPatch",
            &json!({ "text": "# mock_long_info\n" }),
        );
        assert_eq!(s(&long, "/info/title").chars().count(), 200);
        assert_eq!(s(&long, "/info/description").chars().count(), 4000);
        let long_env = mock_reply_with(
            "scheme.previewImportText",
            &json!({ "text": "kind = \"schema_text\"\n# mock_long_info\n" }),
        );
        assert_eq!(s(&long_env, "/description").chars().count(), 4000);
        assert_eq!(
            s(&long_env, "/configPatch/info/description")
                .chars()
                .count(),
            4000
        );
    }

    /// 文本信封两法：内容是信封才成功，否则回 `not_schema_text:` 前缀的错误——
    /// 统一导入通路靠这个前缀回落片段管线，mock 只 Ok 的话离线走不到片段那一支。
    #[test]
    fn mock_schema_text_envelope_detection() {
        let envelope = "[package]\nformat_version = 2\nkind = \"schema_text\"\n";
        let fragment = "[ui.candidate]\nper_page = 9\n";

        let err = super::mock_result("scheme.previewImportText", &json!({ "text": fragment }))
            .expect_err("片段不是信封");
        assert!(
            err.starts_with(crate::pages::config_import::NOT_SCHEMA_TEXT_PREFIX),
            "回落判据是前缀，实际: {err}"
        );

        let v = super::mock_result("scheme.previewImportText", &json!({ "text": envelope }))
            .expect("信封应预览成功");
        assert!(v.get("willAdd").is_some(), "形状须与 previewImport 一致");
        assert!(
            v.pointer("/configPatch/entries")
                .and_then(|e| e.as_array())
                .is_some_and(|a| !a.is_empty()),
            "样例信封应带 configPatch，否则离线核对不到「将修改的配置」区块"
        );

        let imported = super::mock_result("scheme.importText", &json!({ "text": envelope }))
            .expect("信封应导入成功");
        assert!(imported.get("imported").is_some());
        // 单引号写法同样算信封（生成端可自选 TOML 引法）。
        assert!(super::mock_is_schema_text("kind = 'schema_text'"));
    }

    #[test]
    fn mock_shadow_add_rule_ok() {
        assert_eq!(mock_reply("shadow.addRule").get("ok"), Some(&json!(true)));
    }

    #[test]
    fn mock_phrase_system_and_user() {
        let sys = mock_reply("phrase.listSystem");
        assert!(
            sys.as_array()
                .unwrap()
                .iter()
                .all(|p| p.get("isSystem") == Some(&json!(true)))
        );
        let usr = mock_reply("phrase.listUser");
        assert!(usr.get("items").is_some() && usr.get("total").is_some());
        assert_eq!(usr.get("total"), Some(&json!(2)));
        assert!(usr.get("items").unwrap().is_array());
    }

    #[test]
    fn mock_phrase_reset_system() {
        let reset = mock_reply("phrase.resetSystem");
        assert_eq!(reset.get("ok"), Some(&json!(true)));
        assert_eq!(reset.get("changed"), Some(&json!(0)));
    }

    #[test]
    fn mock_phrase_export_import() {
        assert!(mock_reply("phrase.export").get("content").is_some());
        assert_eq!(mock_reply("phrase.import").get("imported"), Some(&json!(0)));
    }

    #[test]
    fn mock_theme_preview_import_and_get_text() {
        let preview = mock_reply("theme.preview");
        assert_eq!(preview.get("display_name"), Some(&json!("测试主题")));
        assert_eq!(preview.get("slug"), Some(&json!("test-theme")));

        let imported = mock_reply("theme.importFromText");
        assert_eq!(imported.get("ok"), Some(&json!(true)));
        assert_eq!(imported.get("slug"), Some(&json!("test-theme")));

        let text = mock_reply("theme.getText");
        assert_eq!(text.get("slug"), Some(&json!("default")));
        assert!(
            text.get("toml")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .contains("[meta]")
        );
    }

    #[test]
    fn mock_get_config_codetable_has_dictionaries() {
        // 码表方案返回带 dictionaries 数组，含主库 + 附加库。
        let cfg = super::mock_reply("schema.getConfig", &json!({ "id": "wubi86" }));
        let dicts = cfg
            .get("dictionaries")
            .and_then(|v| v.as_array())
            .expect("dictionaries 数组");
        assert!(
            dicts.iter().any(|d| d.get("default") == Some(&json!(true))),
            "含主库"
        );
        assert!(
            dicts
                .iter()
                .any(|d| d.get("id") == Some(&json!("wubi86_extra"))),
            "含扩展词库附加库"
        );
    }

    #[test]
    fn mock_get_config_shuangpin_unchanged() {
        // 双拼方案仍返回布局配置、无 dictionaries（保持原行为）。
        let cfg = super::mock_reply("schema.getConfig", &json!({ "id": "double_pinyin" }));
        assert_eq!(
            cfg.pointer("/engine/pinyin/shuangpin/layout"),
            Some(&json!("ziranma"))
        );
    }

    #[test]
    fn mock_set_dict_enabled_persists_within_session() {
        // 保存 wubi86_extra=false 后，getConfig 应读回 enabled=false（mock 会话内记忆）。
        super::mock_reply(
            "schema.setDictEnabled",
            &json!({ "id": "wubi86", "dictId": "wubi86_extra", "enabled": false }),
        );
        let cfg = super::mock_reply("schema.getConfig", &json!({ "id": "wubi86" }));
        let dicts = cfg.get("dictionaries").and_then(|v| v.as_array()).unwrap();
        let extra = dicts
            .iter()
            .find(|d| d.get("id") == Some(&json!("wubi86_extra")))
            .unwrap();
        assert_eq!(
            extra.get("enabled"),
            Some(&json!(false)),
            "保存的开关应被 getConfig 读回"
        );
    }

    #[test]
    fn mock_set_dict_enabled_ok() {
        let r = super::mock_reply(
            "schema.setDictEnabled",
            &json!({ "id": "wubi86", "dictId": "wubi86_extra", "enabled": false }),
        );
        assert_eq!(r.get("ok"), Some(&json!(true)));
        assert_eq!(r.get("live"), Some(&json!(true)));
    }
}

/// 内置 capability 快照（与 src/assets 同一文件），mock 模式离线提供能力契约。
const MOCK_CAPABILITIES: &str = include_str!("assets/capabilities.snapshot.json");
const MOCK_CONFIG: &str = include_str!("mockdata/config.json");

thread_local! {
    /// Mock 会话内的扩展词库开关记忆：(方案 id, 词库 id) → enabled。
    /// 让离线 mock 下「setDictEnabled 保存 → getConfig 重开」能保留改动，仅用于 UI 自测。
    static MOCK_DICT_OVERRIDES: std::cell::RefCell<std::collections::HashMap<(String, String), bool>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// 记录一次 mock setDictEnabled（会话内）。
fn record_mock_dict_override(schema_id: String, dict_id: String, enabled: bool) {
    MOCK_DICT_OVERRIDES.with(|m| m.borrow_mut().insert((schema_id, dict_id), enabled));
}

/// 把会话内记忆的开关套用到 getConfig 返回的 dictionaries[*].enabled 上。
fn apply_mock_dict_overrides(schema_id: &str, cfg: &mut Value) {
    let Some(arr) = cfg.get_mut("dictionaries").and_then(|d| d.as_array_mut()) else {
        return;
    };
    MOCK_DICT_OVERRIDES.with(|m| {
        let map = m.borrow();
        for d in arr.iter_mut() {
            if let Some(did) = d.get("id").and_then(|x| x.as_str()).map(str::to_string)
                && let Some(&en) = map.get(&(schema_id.to_string(), did))
            {
                d["enabled"] = json!(en);
            }
        }
    });
}

thread_local! {
    /// 测试专用：临时顶替 `schema.getConfig` mock 返回的 `followedBehavior.layout`。
    ///
    /// 生产 mock 恒定返回 `"horizontal"`（见下方 `mock_followed_layout`）——离线截图/
    /// 手测不需要这一项会变。只有验证「下拉首项文案随每次打开对话框重算」的用例
    /// （schema_manager.rs `follow_labels_reseeded_on_each_open`）需要在两次
    /// `open_settings_at_id` 之间换一个不同的跟随值，模拟「全局页配置在两次打开之间
    /// 变了」，故给它一个显式的、测试主动摆值的开关，不用不可控的调用计数
    /// （那会让恰好共享同一测试线程的其它用例被悄悄牵连，破坏「测试要确定性」）。
    static MOCK_FOLLOWED_LAYOUT: std::cell::Cell<Option<&'static str>> =
        std::cell::Cell::new(None);
}

/// 供测试设置/清空上面那个顶替值。测试用完必须传 `None` 清空，否则会漏到下一个用例。
#[cfg(test)]
pub(crate) fn set_mock_followed_layout(v: Option<&'static str>) {
    MOCK_FOLLOWED_LAYOUT.with(|c| c.set(v));
}

/// `schema.getConfig` mock 里 `followedBehavior.layout` 的取值：默认恒定 `"horizontal"`，
/// 测试可用 [`set_mock_followed_layout`] 顶替。
fn mock_followed_layout() -> &'static str {
    MOCK_FOLLOWED_LAYOUT
        .with(|c| c.get())
        .unwrap_or("horizontal")
}

/// 统计样例的确定性伪随机源：同一天恒定，故截图可复现（不用 rand，避免引入依赖）。
fn mock_noise(seed: u64, salt: u64) -> u64 {
    let mut x = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(salt.wrapping_mul(1_442_695_040_888_963_407));
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 29;
    x
}

/// 统计页离线样例：区间内逐日生成（含零值日），形状对齐 core `daily_item_json`。
///
/// 刻意做出「工作日多、周末少、偶有整日空档」的分布，否则热力图全是同一档色阶，
/// 截图核对色阶与留白就失去意义。
fn mock_stats_daily(params: &Value) -> Value {
    use chrono::Datelike;
    let get = |k: &str| params.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let parse = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
    let (Some(from), Some(to)) = (parse(get("from")), parse(get("to"))) else {
        return json!([]);
    };

    let mut out = Vec::new();
    let mut cur = from;
    while cur <= to {
        let seed = cur.num_days_from_ce() as u64;
        let weekday = cur.weekday().num_days_from_monday();
        // 8% 的日子整日无输入（休假/未开机）；周末产出约为工作日四成。
        let idle = mock_noise(seed, 1) % 100 < 8;
        let scale = if weekday >= 5 { 0.4 } else { 1.0 };
        let base = if idle {
            0
        } else {
            ((800 + mock_noise(seed, 2) % 3_700) as f64 * scale) as u32
        };

        let chinese = base * 70 / 100;
        let english = base * 18 / 100;
        let punct = base * 8 / 100;
        let other = base - chinese - english - punct;
        let total = base;

        // 时段：上午 9-12、下午 14-18、晚间 20-23 三个峰，其余低谷。
        let mut hours = [0u32; 24];
        if total > 0 {
            let weights: [u32; 24] = [
                0, 0, 0, 0, 0, 0, 1, 2, 4, 9, 10, 9, 5, 6, 9, 10, 9, 8, 5, 4, 7, 8, 6, 3,
            ];
            let wsum: u32 = weights.iter().sum();
            let mut acc = 0u32;
            for h in 0..24 {
                // 末位吸收余数，保证 sum(hours) == total（页面按今日总量算占比）。
                hours[h] = if h == 23 {
                    total - acc
                } else {
                    let v = total * weights[h] / wsum;
                    acc += v;
                    v
                };
            }
        }

        let commit_count = total / 3;
        // 码长以 4 码为主，符合码表方案的实际分布。
        let cld_w = [2u32, 8, 22, 52, 12, 4];
        let mut code_len_dist = [0u32; 6];
        let mut cl_sum = 0u32;
        for (i, w) in cld_w.iter().enumerate() {
            code_len_dist[i] = commit_count * w / 100;
            cl_sum += code_len_dist[i] * (i as u32 + 1);
        }
        let code_len_count: u32 = code_len_dist.iter().sum();
        // 候选位置：首选约 78%，越靠后越少。
        let cpd_w = [78u32, 13, 5, 3, 1];
        let mut cand_pos_dist = [0u32; 5];
        for (i, w) in cpd_w.iter().enumerate() {
            cand_pos_dist[i] = commit_count * w / 100;
        }

        // 活跃秒数反推出 55~85 字/分的速度区间。
        let speed = 55 + mock_noise(seed, 3) % 31;
        let active_seconds = if total > 0 {
            (total as u64 * 60 / speed) as u32
        } else {
            0
        };

        // 双方案共存，便于验证「方案占比」条形图（id 取自 mock 的 schema.list）。
        let wubi = total * 62 / 100;
        let pinyin = total - wubi;
        let by_schema = if total > 0 {
            json!({
                "wubi86": { "tc": wubi, "cn": wubi / 3, "cls": cl_sum * 62 / 100,
                            "clc": code_len_count * 62 / 100, "cpd": cand_pos_dist },
                "pinyin_simp": { "tc": pinyin, "cn": pinyin / 3, "cls": cl_sum * 38 / 100,
                                 "clc": code_len_count * 38 / 100, "cpd": cand_pos_dist },
            })
        } else {
            json!({})
        };

        out.push(json!({
            "d": cur.format("%Y-%m-%d").to_string(),
            "tc": total, "cc": chinese, "ec": english, "pc": punct, "oc": other,
            "h": hours, "cn": commit_count,
            "cls": cl_sum, "clc": code_len_count,
            "cld": code_len_dist, "cpd": cand_pos_dist,
            "as": active_seconds, "bs": by_schema,
            "src": [commit_count, 0, 0],
        }));
        cur += chrono::Duration::days(1);
    }
    json!(out)
}

/// 统计概览样例（17 字段，形状对齐 core `web_stats_summary`）。
///
/// 由 `mock_stats_daily` 的同一份样例汇总而来——两个接口若各编各的，
/// 页面上「今日输入」和热力图最后一格会对不上，离线核对反而误导。
fn mock_stats_summary() -> Value {
    let today = chrono::Local::now();
    let from = (today - chrono::Duration::days(180))
        .format("%Y-%m-%d")
        .to_string();
    let to = today.format("%Y-%m-%d").to_string();
    let days = mock_stats_daily(&json!({ "from": from, "to": to }));
    let days = days.as_array().cloned().unwrap_or_default();
    let num = |d: &Value, k: &str| d.get(k).and_then(|v| v.as_u64()).unwrap_or(0);

    let last = days.last().cloned().unwrap_or_else(|| json!({}));
    let today_chars = num(&last, "tc");
    let window_total: u64 = days.iter().map(|d| num(d, "tc")).sum();
    let active_days = days.iter().filter(|d| num(d, "tc") > 0).count() as u64;
    // 窗口外还有更早的历史，累计量取窗口和的 1.6 倍，避免「累计 == 半年」的失真。
    let total_chars = window_total * 8 / 5;

    // 连续天数：从今天往回数到第一个空白日。
    let streak_current = days.iter().rev().take_while(|d| num(d, "tc") > 0).count() as u64;
    let streak_max = streak_current.max(21);

    let (max_day_chars, max_day_date) = days.iter().fold((0u64, String::new()), |acc, d| {
        let t = num(d, "tc");
        if t > acc.0 {
            (
                t,
                d.get("d")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        } else {
            acc
        }
    });

    let week_chars: u64 = days.iter().rev().take(7).map(|d| num(d, "tc")).sum();
    let month_chars: u64 = days.iter().rev().take(30).map(|d| num(d, "tc")).sum();
    let cl_sum: u64 = days.iter().map(|d| num(d, "cls")).sum();
    let cl_cnt: u64 = days.iter().map(|d| num(d, "clc")).sum();
    let first_sel: u64 = days
        .iter()
        .map(|d| {
            d.get("cpd")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        })
        .sum();
    let cand_sel: u64 = days
        .iter()
        .map(|d| {
            d.get("cpd")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_u64()).sum::<u64>())
                .unwrap_or(0)
        })
        .sum();
    let active_sec: u64 = days.iter().map(|d| num(d, "as")).sum();
    let speed = |chars: u64, secs: u64| if secs == 0 { 0 } else { chars * 60 / secs };

    json!({
        "today_chars": today_chars,
        "today_chinese": num(&last, "cc"),
        "today_english": num(&last, "ec"),
        "total_chars": total_chars,
        "active_days": active_days,
        "daily_avg": if active_days > 0 { total_chars / active_days } else { 0 },
        "streak_current": streak_current,
        "streak_max": streak_max,
        "week_chars": week_chars,
        "month_chars": month_chars,
        "max_day_chars": max_day_chars,
        "max_day_date": max_day_date,
        "avg_code_len": if cl_cnt > 0 { cl_sum as f64 / cl_cnt as f64 } else { 0.0 },
        "first_select_rate": if cand_sel > 0 { first_sel as f64 / cand_sel as f64 } else { 0.0 },
        "today_speed": speed(today_chars, num(&last, "as")),
        "overall_speed": speed(window_total, active_sec),
        "max_speed": 118,
    })
}

/// mock 出码：每字取一个占位字母；含 BMP 外字符（扩展区生僻字）时回空串，
/// 用来复现真实出码里「方案未配拆字 → 出不了码」这一支。
fn mock_word_code(text: &str) -> String {
    if text.is_empty() || text.chars().any(|c| c as u32 > 0xFFFF) {
        return String::new();
    }
    text.chars()
        .zip("abcdefghijklmnopqrstuvwxyz".chars().cycle())
        .map(|(_, letter)| letter)
        .collect()
}

/// Mock 模式的完整结果：**先判错再取值**。
///
/// 没有这一层的话 mock 只会 `Ok`，而统一导入通路恰恰靠「`scheme.previewImportText`
/// 报 `not_schema_text:`」来回落片段管线——离线模式下每段文本都会被当成方案信封，
/// 片段导入整条路走不到。
fn mock_result(method: &str, params: &Value) -> Result<Value, String> {
    #[cfg(test)]
    MOCK_CALLS.with(|c| c.borrow_mut().push((method.to_string(), params.clone())));
    match mock_error(method, params) {
        Some(e) => Err(e),
        None => Ok(mock_reply(method, params)),
    }
}

#[cfg(test)]
thread_local! {
    /// Mock 模式下按序记录每次调用的 (method, params)。
    ///
    /// 返回值对不对是一回事，**请求里少个字段**是另一回事——后者只有接上真实 core
    /// 才会暴露，本地怎么点都是绿的。多步编排（导入两法 + applyPatch）尤其需要锁住
    /// 发出去的参数形状与先后顺序。mock 分支不 spawn 线程，与调用方同线程，故
    /// thread_local 够用。
    static MOCK_CALLS: std::cell::RefCell<Vec<(String, Value)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
thread_local! {
    /// 测试用：让某个方法在 mock 下强制返回错误（覆盖 `mock_error` 里的常规判定）。
    static FORCED_ERRORS: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// 测试用：让 `method` 在 mock 下返回 `Err(msg)`；`msg = None` 撤销。
#[cfg(test)]
pub(crate) fn force_mock_error(method: &str, msg: Option<&str>) {
    FORCED_ERRORS.with(|f| {
        let mut f = f.borrow_mut();
        f.retain(|(m, _)| m != method);
        if let Some(msg) = msg {
            f.push((method.to_string(), msg.to_string()));
        }
    });
}

/// 取走并清空本线程记录的 mock 调用（取走语义：便于「清场 → 触发 → 断言」）。
#[cfg(test)]
pub(crate) fn take_mock_calls() -> Vec<(String, Value)> {
    MOCK_CALLS.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// mock 侧会「失败」的调用。目前只有文本信封两法：内容不是信封时按跨仓契约回
/// `not_schema_text:` 前缀的错误（见 `package-format.md` §3.4）。
fn mock_error(method: &str, params: &Value) -> Option<String> {
    #[cfg(test)]
    if let Some(e) = FORCED_ERRORS.with(|f| {
        f.borrow()
            .iter()
            .find(|(m, _)| m == method)
            .map(|(_, e)| e.clone())
    }) {
        return Some(e);
    }
    match method {
        "scheme.previewImportText" | "scheme.importText" => {
            let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
            (!mock_is_schema_text(text))
                .then(|| "not_schema_text: 缺少 package.kind = \"schema_text\"".to_string())
        }
        // 逐条删除中途失败：词条文本为此记号时 `*.remove` 报错，供离线走通「删到一半」那条路。
        m if m.ends_with(".remove") => (params.get("text").and_then(|v| v.as_str())
            == Some(MOCK_FAIL_TEXT))
        .then(|| "mock: 删除失败".to_string()),
        // compat.*：进程名为此记号时写方法报错（形状照核心真实的补丁被拒信息），
        // 供离线走通「写失败 → toast 核心的中文报错」那条路。
        // compat.import：内容里带 MOCK_FAIL_TEXT 时预览 / 提交都报错（模拟核心断线 / 拒绝）。
        "compat.import"
            if params
                .get("content")
                .and_then(|v| v.as_str())
                .is_some_and(|c| c.contains(MOCK_FAIL_TEXT)) =>
        {
            Some("mock: 导入失败".to_string())
        }
        m if m.starts_with("compat.")
            && params.get("process").and_then(|v| v.as_str()) == Some(MOCK_COMPAT_FAIL_PROCESS) =>
        {
            Some("字段 first_show_mode 的值 \"bogus\" 无效".to_string())
        }
        _ => None,
    }
}

/// 见 [`mock_error`]：`compat.*` 的 `process` 为此值时 mock 报错。
pub(crate) const MOCK_COMPAT_FAIL_PROCESS: &str = "fail.exe";

/// 见 [`mock_error`]：`*.remove` 的 `text` 为此值时 mock 报错。
pub(crate) const MOCK_FAIL_TEXT: &str = "mock_fail";

/// mock 的信封判定：只看 `kind` 是否声明为 `schema_text`（真实解析在 core 侧，
/// 这里只需分出「信封 / 不是信封」两支供离线走通两条通路）。
fn mock_is_schema_text(text: &str) -> bool {
    text.contains("kind = \"schema_text\"") || text.contains("kind = 'schema_text'")
}

/// 导入文本里带这个记号时，mock 把说明（§2.3）撑到 core 限额上限。
///
/// 说明是分发者提供的任意文本，`description` 限额 4000 字符——足以把对话框顶出
/// 屏幕。样例说明都是两三行的短文本，光靠它们离线怎么点都看不出封顶有没有生效。
const MOCK_LONG_INFO_MARK: &str = "mock_long_info";

fn mock_wants_long_info(params: &Value) -> bool {
    params
        .get("text")
        .and_then(|v| v.as_str())
        .is_some_and(|t| t.contains(MOCK_LONG_INFO_MARK))
}

/// core 限额上限的说明：title 恰 200 字符、description 恰 4000 字符。
fn mock_limit_info() -> Value {
    let title: String = "分发者可以把标题一直写到限额上限而不报错，"
        .chars()
        .cycle()
        .take(200)
        .collect();
    let mut desc = String::new();
    let mut i = 1;
    while desc.chars().count() < 4000 {
        desc.push_str(&format!("第 {i} 行说明：分发者提供的任意文本。\n"));
        i += 1;
    }
    let description: String = desc.chars().take(4000).collect();
    json!({ "title": title, "description": description })
}

/// mock 侧判断导入目标是不是拼音族方案（按 schemaId 命名约定）。
///
/// 只服务于 mock 的形状分流：真实 core 走 `schema_engine_type`，与本函数无关。
fn mock_target_is_pinyin(params: &Value) -> bool {
    params
        .get("schemaId")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s.contains("pinyin") || s.contains("shuangpin"))
}

/// `appearance.previewTemplate` 的离线回包（设置端模板预览行）。
///
/// 真实求值要变量样例与主题，都在 core；离线只求**形状与坐标系**对：文字是固定样例（有 `$[`
/// 时给一段强调色），`$[…]` 里既不是 `#` 色值也不是标准色名的，按 core 的口径报一条问题——
/// 区间是**模板**里的字节偏移，截图里能看到问题落回输入框的哪一段。
fn mock_preview_template(params: &Value) -> Value {
    const NAMES: &[(&str, [u8; 4], [u8; 4])] = &[
        ("text", [30, 30, 30, 255], [255, 255, 255, 255]),
        ("text_dim", [100, 100, 100, 255], [189, 189, 189, 255]),
        ("text_hint", [150, 150, 150, 255], [160, 160, 160, 255]),
        ("accent", [66, 133, 244, 255], [138, 180, 248, 255]),
        ("accent_text", [37, 99, 235, 255], [138, 180, 248, 255]),
        ("success", [30, 142, 62, 255], [129, 201, 149, 255]),
        ("warning", [176, 96, 0, 255], [253, 214, 99, 255]),
        ("error", [217, 48, 37, 255], [242, 139, 130, 255]),
        ("info", [26, 115, 232, 255], [138, 180, 248, 255]),
    ];
    let tpl = params
        .get("template")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tooltip = params.get("scene").and_then(|v| v.as_str()) != Some("comment");
    let pick = |name: &str| {
        NAMES
            .iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, c, t)| if tooltip { *t } else { *c })
    };
    let mut problems = Vec::new();
    let mut from = 0;
    while let Some(i) = tpl[from..].find("$[").map(|i| i + from) {
        let spec_at = i + 2;
        let Some(len) = tpl[spec_at..].find(']') else {
            break;
        };
        let spec = &tpl[spec_at..spec_at + len];
        let hex_ok = spec.strip_prefix('#').is_some_and(|h| {
            matches!(h.len(), 3 | 6 | 8) && h.chars().all(|c| c.is_ascii_hexdigit())
        });
        if !hex_ok && pick(spec).is_none() {
            problems.push(json!({
                "start": spec_at, "end": spec_at + len,
                "message": format!("当前主题没有颜色「{spec}」，按正文色显示"),
            }));
        }
        from = spec_at + len;
    }
    let scene = params.get("scene").and_then(|v| v.as_str()).unwrap_or("");
    let (text, runs) = if scene == "label" {
        // 段名多是字面文字：原样回显（去掉 `$[…]{` 与配对的 `}` 太费事，离线不求这个）。
        (
            tpl,
            json!([{ "start": 0, "end": tpl.len(), "rgba": pick("accent_text") }]),
        )
    } else if tooltip {
        (
            "你：nǐ\n好：hǎo/hào",
            json!([{ "start": 6, "end": 9, "rgba": pick("error") }]),
        )
    } else {
        (
            "hao (nǐ hǎo)",
            json!([{ "start": 4, "end": 14, "rgba": pick("accent_text") }]),
        )
    };
    let runs = if tpl.contains("$[") { runs } else { json!([]) };
    let runs = &runs;
    let swatches: serde_json::Map<String, Value> = params
        .get("swatches")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .map(|n| {
            (
                n.to_string(),
                json!(pick(n).unwrap_or([150, 150, 150, 255])),
            )
        })
        .collect();
    json!({
        "text": text,
        "fg": if tooltip { [255, 255, 255, 255] } else { [150, 150, 150, 255] },
        "bg": if tooltip { [60, 60, 60, 240] } else { [255, 255, 255, 255] },
        "runs": runs,
        "selected": if tooltip { Value::Null } else {
            json!({ "fg": [30, 30, 30, 255], "bg": [235, 242, 254, 255], "runs": runs })
        },
        "problems": problems,
        "swatches": swatches,
    })
}

/// Mock 模式：返回形状合理的默认值；manifest/config 用内置真实数据，
/// 方案/主题/字体/系统信息给代表性样例，便于离线核对 UI 保真度。
fn mock_reply(method: &str, params: &Value) -> Value {
    match method {
        "system.capabilities" => {
            serde_json::from_str(MOCK_CAPABILITIES).unwrap_or_else(|_| json!({}))
        }
        "config.get" => serde_json::from_str(MOCK_CONFIG).unwrap_or_else(|_| json!({})),
        // ★ 与 `system.info` 的 `customEdition` 相反，这里给的是**健康态**：定制版是一种
        // 正常形态，配置降级是故障。让 mock 默认带一条红色横幅，等于每次开发截图都要
        // 先无视它——而「学会无视警告」正是这条横幅最怕的结局。
        // 要验降级顶栏，把 degraded 改成 true 并填 sections（验完记得改回来）。
        // 刻意是**健康态**：降级是故障态，不该成为开发机上的默认景象（与 `customEdition`
        // 的取舍相反——那个非 null 是因为它是唯一的自查途径，而这个的自查走单元测试）。
        // `unparsable` 仍要给空数组：形态恒定是跨仓契约的一部分，字段缺失与
        // 「这版 core 还没实现」在客户端侧无从区分。
        "config.degradation" => json!({
            "degraded": false, "sections": [], "totalFallback": false, "unparsable": []
        }),
        // 注释词库的可选来源。id 里的 `auto:` 前缀与 core 的 `auto_comment_id` 同源
        // ——本屏靠它把自动派生项与手工项分开，前缀写错的表现是「自动项变成可删的手工项」。
        // 两个 files 一有一无 comment 列：缺列的那份要能看到「缺 comment 列」那行说明。
        "comment.sources" => json!({
            "auto": [
                { "id": "auto:pinyin/rime_frost.dict.yaml", "label": "朗月拼音词库",
                  "rel": "pinyin/rime_frost.dict.yaml", "schemas": ["pinyin_simp"] }
            ],
            "files": [
                { "path": "comments/en.dict.yaml", "label": "简明英汉", "hasComment": true },
                { "path": "comments/emoji.dict.yaml", "label": "Emoji 名称", "hasComment": true },
                { "path": "comments/broken.dict.yaml", "label": "", "hasComment": false }
            ]
        }),
        "schema.list" => json!([
            { "id": "wubi86", "name": "五笔 86", "engineType": "codetable", "scheme": "",
              "builtin": true, "version": "2.1.0", "description": "标准五笔 86 版码表方案", "author": "王永民" },
            { "id": "wubi98", "name": "五笔 98", "engineType": "codetable", "scheme": "",
              "builtin": true, "version": "1.0.0", "description": "标准五笔 98 版码表方案" },
            { "id": "wubi_mixed", "name": "五笔拼音混输", "engineType": "mixed", "scheme": "",
              "builtin": true, "version": "1.2.0", "description": "五笔 86 + 拼音混合输入" },
            { "id": "pinyin_simp", "name": "全拼·简", "engineType": "pinyin", "scheme": "full",
              "builtin": true, "description": "简体全拼方案" },
            { "id": "double_pinyin", "name": "双拼", "engineType": "pinyin", "scheme": "shuangpin",
              "builtin": false, "version": "0.5.0", "description": "双拼输入方案（布局可在设置中切换）" }
        ]),
        // 当前活跃方案（core 的运行时状态）。取值必须在上面的 schema.list 内，
        // 否则 mock 模式下「当前」徽章会无处落脚。
        "schema.active" => json!({ "id": "wubi86" }),
        // mock 无状态：只回 ok，UI 侧自行更新 active_id（真实 core 才有切换副作用）。
        "schema.setActive" => json!({ "ok": true }),
        // 双拼布局清单：core 侧合并扫描 schemas/shuangpin/*.toml（安装+用户目录）后返回。
        "schema.layouts" => json!([
            { "id": "abc", "name": "智能ABC" },
            { "id": "mspy", "name": "微软双拼" },
            { "id": "sogou", "name": "搜狗双拼" },
            { "id": "xiaohe", "name": "小鹤双拼" },
            { "id": "ziguang", "name": "紫光双拼" },
            { "id": "ziranma", "name": "自然码" }
        ]),
        "theme.list" => json!([
            // 首项 id 必须与 mock config 的 `ui.theme.name` 对得上：对不上时主题下拉选不中
            // 当前值、回落首项，于是「什么都没动」也会写出一个不同的值 ⇒ is_dirty 恒为真。
            { "name": "default", "display_name": "清风·浅", "version": "3.2.1", "builtin": true },
            { "name": "jade", "display_name": "青玉", "version": "3.2.1", "builtin": true },
            { "name": "mono", "display_name": "墨", "version": "1.0.0", "builtin": false }
        ]),
        "theme.delete" => json!({ "ok": true }),
        "appearance.previewTemplate" => mock_preview_template(params),
        // 全局键 → 覆盖它的方案 id（core `schema.overrideSummary`）。
        "schema.overrideSummary" => json!({ "ui.candidate.layout": ["wubi86"] }),
        "theme.resolved" => json!({
            "themeId": "default", "fontSize": 18, "fontFamily": null, "fontWeight": 0,
            "pagerBarDisplay": "auto", "pageNumberDisplay": "show",
            "langbarText": ["#000000", "#FFFFFF", "#000000", "#FFFFFF"],
        }),
        // 形状与 core `wind-rpc` dispatch 的 `system.fonts` 一致（`family` + `display_name`）。
        "system.fonts" => json!([
            { "family": "Microsoft YaHei", "display_name": "微软雅黑" },
            { "family": "Source Han Sans SC", "display_name": "思源黑体" },
            { "family": "DengXian", "display_name": "等线" }
        ]),
        // ★ `customEdition` 刻意给成非 null：定制版徽章（关于页）在开发机上**永远**渲染不
        // 出来——开发机装的不是定制版，core 那边恒返回 null。mock 是这个分支唯一的自查
        // 途径，与下面词库/统计给非空样例是同一个理由。要看「非定制版」那一态，把这个键
        // 改成 null 或整个删掉即可（两者对 UI 等价，见 `custom_edition_badge`）。
        "system.info" => json!({
            "version": "3.2.1", "platform": "windows", "arch": "x86_64",
            "build": "release", "dataDir": "C:/Users/.../WindInput", "running": true,
            "customEdition": {
                "id": "huma-edition", "name": "虎码定制版",
                "version": "1.2", "baseVersion": "0.119.0"
            }
        }),
        "dict.stats" => json!([]),
        "stats.daily" => mock_stats_daily(params),
        // 词库管理离线样例：便于截图核对非空界面（形状对齐 core webdata.rs）。
        // 字符类：覆盖三态的每一档，外加「有逐字覆盖压着」那一行——那是 §7.2 说的
        // 「改了默认判定却没反应」的现成场景，开发时看得见才不会把提示做丢。
        "charset.list" => json!([
            { "key": "emoji", "name": "Emoji 表情", "ranges": [], "memberCount": 1427,
              "order": 10, "default": null, "noFreq": false, "inRare": false,
              "enabled": true, "builtin": false, "overridden": false, "overrideCount": 1204 },
            { "key": "common_han", "name": "常用汉字", "ranges": [], "memberCount": 8104,
              "order": 50, "default": true, "noFreq": false, "inRare": false,
              "enabled": true, "builtin": false, "overridden": false, "overrideCount": 0 },
            { "key": "我的符号", "name": "我不要的符号", "ranges": ["U+2600-U+26FF"],
              "memberCount": 3, "order": 100, "default": false, "noFreq": true,
              "inRare": false, "enabled": true, "builtin": false, "overridden": true,
              "overrideCount": 0 },
            { "key": "表情符号", "name": "表情符号", "ranges": ["U+1F300-U+1FAFF"],
              "memberCount": 0, "order": 900, "default": null, "noFreq": false,
              "inRare": false, "enabled": true, "builtin": true, "overridden": false,
              "overrideCount": 0 },
            { "key": "注音符号", "name": "注音符号", "ranges": ["U+3100-U+312F"],
              "memberCount": 0, "order": 900, "default": null, "noFreq": false,
              "inRare": true, "enabled": false, "builtin": true, "overridden": true,
              "overrideCount": 2 }
        ]),
        "charset.set" | "charset.reset" | "charset.delete" => json!({ "ok": true }),
        "charset.clearRedundant" => json!({ "scanned": 1204, "removed": 1198 }),
        // 外部编辑 / 新建类：core 把文件写到临时目录、回路径；设置页负责打开。
        // 「外部编辑」三件套（见 pages/dict/external_edit.rs）。探测只报路径与现状，不写。
        "charset.editFile" => {
            let key = params
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("my_class_1");
            let path = std::env::temp_dir()
                .join("WindInput")
                .join("charsets")
                .join(format!("{key}.yaml"));
            json!({
                "key": key,
                "name": if key == "my_class_1" { "我的字符类" } else { "Emoji 表情" },
                "builtin": key != "my_class_1",
                "path": path.to_string_lossy(),
                "exists": path.is_file(),
                "modified": if path.is_file() { Value::from("2026-09-07 11:20") } else { Value::Null },
            })
        }
        "charset.exportEdit" => {
            let key = params
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("my_class_1");
            let path = std::env::temp_dir()
                .join("WindInput")
                .join("charsets")
                .join(format!("{key}.yaml"));
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            let _ = std::fs::write(
                &path,
                "# ⚠️ 此文件不会被自动读取。改完保存后，回到设置页「字符集分类」用「从文件加载」导入。\n---\nkey: ".to_string() + key + "\n...\n★\n",
            );
            json!({ "path": path.to_string_lossy() })
        }
        "charset.importFile" => json!({ "classes": [
            { "key": "emoji", "name": "Emoji 表情", "builtin": true, "added": 1, "removed": 2, "fields": 1 }
        ] }),
        "phrase.list" => json!([
            { "code": "bj", "text": "北京", "position": 0, "weight": 1000, "enabled": true, "isSystem": true },
            { "code": "yx", "text": "example@example.com", "position": 0, "weight": 1000, "enabled": true, "isSystem": true },
            { "code": "dd", "text": "（＾＿＾）", "position": 0, "weight": 1000, "enabled": false, "isSystem": false }
        ]),
        "phrase.listSystem" => json!([
            { "code": "rq", "text": "$date", "weight": 1000, "position": 0, "enabled": true, "isSystem": true },
            { "code": "dd", "text": "（＾＿＾）", "weight": 1000, "position": 0, "enabled": false, "isSystem": true }
        ]),
        "phrase.listUser" => json!({ "items": [
            { "code": "bj", "text": "北京", "weight": 1000, "position": 0, "enabled": true, "isSystem": false },
            { "code": "yx", "text": "example@example.com", "weight": 1000, "position": 0, "enabled": true, "isSystem": false }
        ], "total": 2 }),
        "phrase.export" => {
            json!({ "content": "# WindInput 用户数据文件\nwind_dict:\n  version: 1\n\n--- !phrases\nbj\t北京\t1000\t0\t1\n" })
        }
        "phrase.import" => json!({ "imported": 0, "skipped": 0 }),
        // 短语分发格式预览。夹具覆盖四种行态：普通、命令、遮蔽系统短语，
        // 以及带反斜杠笔误提示的——只给普通条目的话，提示行与遮蔽标注的
        // 渲染路径在 mock 模式下永远走不到。
        "phrase.previewImportText" => json!({
            "title": "示例短语包",
            "entries": [
                { "line": 2, "code": "kx", "text": "(＾▽＾)", "status": "new",
                  "hints": [], "error": null },
                { "line": 3, "code": "jsb", "text": "$CC(\"记事本\", proc.run(\"notepad.exe\"))",
                  "status": "new", "hints": [], "error": null },
                { "line": 4, "code": "yx", "text": "example@example.com",
                  "status": "shadowsSystem", "hints": [], "error": null },
                { "line": 5, "code": "dk", "text": "$CC(\"打开\", open(\"D:notes\"))",
                  "status": "new",
                  "hints": [{ "kind": "controlCharInPath", "func": "open",
                              "message": "open 的路径里出现了换行或制表符，通常是反斜杠只写了一个" }],
                  "error": null }
            ],
            "problems": [],
            "counts": { "new": 3, "existsUser": 0, "shadowsSystem": 1, "skippedLines": 0 },
        }),
        "phrase.importText" => json!({
            "added": 0, "skippedExisting": 0, "shadowedSystem": 0,
            "skippedInvalid": 0, "skippedUnselected": 0,
        }),
        "phrase.resetSystem" => json!({ "ok": true, "changed": 0 }),
        // 快捷输入格式表。夹具刻意覆盖四种状态：普通行、被停用的行（`moveIndex` 为 null，
        // 调序按钮应置灰）、调整过的行（`adjusted` 为真，「恢复此条」才可点）、以及**用户
        // 自己加的行**（`user` 为真，可改模板/可删、但「恢复此条」不可点）。
        // 只给两个类别即可——UI 侧的类别集是从数据里现取的，不写死五个。
        "quick.list" => json!([
            { "kind": "date", "id": "date.cn", "text": "$Y年$M月$D日", "displayPos": 1,
              "moveIndex": 0, "enabled": true, "adjusted": false, "user": false, "sample": "2026年8月17日" },
            { "kind": "date", "id": "date.iso", "text": "$YYYY-$MM-$DD", "displayPos": 2,
              "moveIndex": 1, "enabled": true, "adjusted": true, "user": false, "sample": "2026-08-17" },
            { "kind": "date", "id": "date.basic", "text": "$YYYY$MM$DD", "displayPos": 3,
              "moveIndex": null, "enabled": false, "adjusted": true, "user": false, "sample": "20260817" },
            { "kind": "date", "id": "date.u1", "text": "$Y/$M/$D", "displayPos": 4,
              "moveIndex": 2, "enabled": true, "adjusted": false, "user": true, "sample": "2026/8/17" },
            { "kind": "number", "id": "number.amount", "text": "$AMT", "displayPos": 1,
              "moveIndex": 0, "enabled": true, "adjusted": false, "user": false, "sample": "壹仟贰佰叁拾肆元伍角整" }
        ]),
        "quick.move" | "quick.setEnabled" | "quick.resetEntry" | "quick.resetKind"
        | "quick.setText" | "quick.delete" => json!({ "ok": true }),
        "quick.add" => json!({ "ok": true, "id": "date.u2" }),
        // 变量提示：只给两类、每类两条即可——真值来自 core，这里只让离线预览有东西可显。
        "quick.vars" => json!([
            { "kind": "date", "vars": [
                { "name": "Y", "desc": "年，如 2026" },
                { "name": "LMD", "desc": "农历月日，四月廿九" }
            ]},
            { "kind": "number", "vars": [
                { "name": "AMT", "desc": "金额大写，壹仟贰佰叁拾肆元伍角" }
            ]}
        ]),
        "quick.export" => json!({
            "content": "# WindInput 快捷输入 · 用户设置\n\n[[formats]]\nid = 'date.u1'\nkind = 'date'\ntext = '$Y/$M/$D'\nposition = 0\n\n[[adjust]]\nkind = 'date'\ndisabled = ['date.basic']\n"
        }),
        "quick.previewImport" => {
            json!({ "moved": 1, "disabled": 1, "formats": 1, "skipped": [], "kinds": ["date"] })
        }
        "quick.import" => json!({ "moved": 1, "disabled": 1, "formats": 1, "skipped": [] }),
        "temp.list" => json!([
            { "code": "nihao", "text": "你好", "count": 3 },
            { "code": "women", "text": "我们", "count": 7 }
        ]),
        // 设置页列表走分页版（`{items,total}`）；上面那条裸数组留给全量取用方。
        "temp.listPaged" => json!({ "items": [
            { "code": "nihao", "text": "你好", "count": 3, "lastUsed": 1_758_000_000 },
            { "code": "women", "text": "我们", "count": 7, "lastUsed": 1_750_000_000 }
        ], "total": 2 }),
        "shadow.list" => json!([
            { "code": "wm", "word": "我们", "candId": "c1", "type": "pin", "position": 0 },
            { "code": "nh", "word": "你好", "candId": null, "type": "delete" }
        ]),
        "shadow.listPaged" => json!({ "items": [
            { "code": "wm", "word": "我们", "candId": "c1", "type": "pin", "position": 0 },
            { "code": "nh", "word": "你好", "candId": null, "type": "delete" }
        ], "total": 2 }),
        // 返回撤销条数（与 temp.clear 同形）。
        "shadow.clear" => json!(2),
        "theme.preview" => json!({
            "slug": "test-theme",
            "display_name": "测试主题",
            "name": "测试主题",
            "version": "1.0.0",
            "author": "WindInput"
        }),
        // 主题文本里含哨兵 MOCK_EXISTING 且未 force ⇒ 回同名冲突回执，供覆盖确认链路的
        // 单测使用。真 core 的判据是目录已存在，mock 无文件系统，只能靠哨兵表达这个分支。
        "theme.importFromText" => {
            let force = params
                .get("force")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let text = params
                .get("yaml")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if !force && text.contains("MOCK_EXISTING") {
                json!({
                    "ok": false,
                    "conflict": true,
                    "slug": "test-theme",
                    "display_name": "测试主题",
                    "error": "主题「测试主题」已存在"
                })
            } else {
                json!({
                    "ok": true,
                    "slug": "test-theme",
                    "display_name": "测试主题"
                })
            }
        }
        // 主题包：路径里含 MOCK_EXISTING 视作撞名，与 importFromText 的哨兵同规矩，
        // 于是「撞名转覆盖确认」那条判断在两种来源上能用同一套用例验。
        "theme.previewPackage" => json!({
            "display_name": "测试主题",
            "author": "WindInput",
            "version": "1.0",
            "asset_count": 2,
            "has_preview": true
        }),
        "theme.importPackage" => {
            let force = params
                .get("force")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let path = params
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if !force && path.contains("MOCK_EXISTING") {
                json!({
                    "ok": false,
                    "conflict": true,
                    "slug": "test-theme",
                    "display_name": "测试主题",
                    "error": "主题「测试主题」已存在"
                })
            } else {
                // slug 与 importFromText 那条**刻意不同**：两种来源走错方法时，光看
                // 「成功了」是分不出来的（哨兵在两边同样生效），得靠回执认出走的是哪条。
                json!({
                    "ok": true,
                    "slug": "test-theme-pkg",
                    "display_name": "测试主题（包）",
                    "files": ["theme.toml", "assets/bg.png"],
                    "reloaded": false
                })
            }
        }
        "theme.exportPackage" => json!({
            "ok": true,
            "path": params
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("C:\\mock\\theme.wtheme"),
            // 展示名刻意与 importPackage 那条不同:两条路走错时光看「成功了」分不出来,
            // 得靠回执认出走的是哪条(同 importPackage 上方那段的取舍)。
            "display_name": "测试主题（导出）",
            "asset_count": 2,
            "has_preview": true
        }),
        "theme.getText" => json!({
            "slug": "default",
            "toml": "base = \"_qingfeng\"\n\n[meta]\nname = \"清风·蓝\"\nversion = \"1.0\"\nauthor = \"清风输入法\"\n"
        }),
        // mockdata 就是「无用户层的系统预置」，正是 getDefaults 的语义，故直接复用。
        // 此前这里返回空对象，Config::from_value({}) 拿不到任何值——mock 模式下
        // 「恢复默认」是静默失效的，点了没反应也没有报错。
        "config.getDefaults" => serde_json::from_str(MOCK_CONFIG).unwrap_or_else(|_| json!({})),
        "schema.references" => {
            json!({})
        }
        // 按方案 id 分流：码表方案返回 dictionaries（含主库 + 三个附加库，
        // 对齐 data/schemas/wubi86.schema.toml）；其余（双拼等）保持布局样例。
        "schema.getConfig" => {
            let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
            // 码表族方案（五笔各版，排除混输）返回 dictionaries 样例；"codetable" 为
            // 未来通用码表 id 预留分支（当前 schema.list 未含此 id）。
            let is_codetable_family =
                (id.starts_with("wubi") && id != "wubi_mixed") || id == "codetable";
            // 「跟随全局（值）」的值（core 只读旁路字段，两个分支都带）。
            // auxEnabled/auxMaxPhraseLen/auxDirect 给非默认值（开 / 4 / 开）：拼音族分支下面带了
            // `aux_code.files`，本节会渲染出来，得看得出跟随值不是零值才有目视意义。
            let followed = json!({
                "layout": mock_followed_layout(), "fontFamily": "微软雅黑",
                "auxEnabled": true, "auxMaxPhraseLen": 4, "auxDirect": true
            });
            if is_codetable_family {
                let mut v = json!({
                    "schema": { "id": id, "name": "五笔 86" },
                    "engine": { "type": "codetable" },
                    "followedBehavior": followed,
                    "dictionaries": [
                        { "id": "wubi86_main",  "label": "极点五笔主词库", "description": "含单字与高频词组", "default": true },
                        { "id": "wubi86_extra", "label": "扩展词库", "description": "新词、网络用语、生僻词等", "default_enabled": true },
                        { "id": "wubi86_emoji", "label": "Emoji 表情", "description": "按五笔编码检索 emoji", "default_enabled": true, "enabled": false },
                        { "id": "wubi86_xzqy",  "label": "行政区域", "description": "省市县区等地名词条", "default_enabled": false }
                    ]
                });
                // 套用本会话经 setDictEnabled 记忆的开关，令离线也能验证「保存后重开保留」。
                apply_mock_dict_overrides(id, &mut v);
                v
            } else {
                json!({
                    "engine": { "type": "pinyin", "pinyin": {
                        "scheme": "shuangpin", "shuangpin": { "layout": "ziranma" }
                    },
                        // 拼音族有辅助码表（对齐真实方案，如全拼配笔画、双拼配小鹤形码）——
                        // 给个非空 files 让 `has_aux_code_table` 判真，`SettingsSection::AuxCode`
                        // 才在 `WIND_RPC_MOCK=1` 下有得看，不然这一节永远建不出来。
                        "aux_code": { "files": ["aux_code/bihua.txt"] }
                    },
                    // 方案文件自己声明的来源（core 旁路字段，不含 override）。与上面合并后的
                    // `aux_code.files` 相同 ⇒ 「辅助码来源」那一行默认呈现为跟随。
                    "auxCodeBaseFiles": ["aux_code/bihua.txt"],
                    "followedBehavior": followed
                })
            }
        }
        // 辅助码来源的可选项（对齐核心 `schema.auxCodeSources`）：码表方案 + aux_code/*.txt。
        // 名字给中文，下拉与跟随档文字在 `WIND_RPC_MOCK=1` 下才有得看。
        "schema.auxCodeSources" => json!({
            "schemas": [
                { "id": "wubi86", "name": "五笔 86" },
                { "id": "stroke", "name": "笔画" }
            ],
            "files": [
                { "path": "aux_code/bihua.txt", "label": "笔画" },
                { "path": "aux_code/flypy_full.txt", "label": "小鹤" },
                { "path": "aux_code/ZRM-wanxiang.txt", "label": "自然码" }
            ]
        }),
        "schema.setDictEnabled" => {
            let id = params
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let dict_id = params
                .get("dictId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let enabled = params
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            record_mock_dict_override(id, dict_id, enabled);
            json!({ "ok": true, "live": true })
        }
        "schema.saveConfig" => json!({ "ok": true }),
        "system.status" => json!({ "running": true }),
        "stats.summary" => mock_stats_summary(),
        "stats.clear" => json!({ "ok": true }),
        "stats.pruneBefore" => json!({ "count": 0, "before": "" }),
        "dict.listPaged" => json!({ "items": [
            { "code": "aaaa", "text": "工", "weight": 0 },
            { "code": "ggkg", "text": "王中", "weight": 5 },
            { "code": "wghg", "text": "程序", "weight": 3 }
        ], "total": 3 }),
        // lastUsed 为 unix 秒（与真实 core 返回同形态，前端格式化为本地时间显示）。
        "freq.listPaged" => json!({ "items": [
            { "code": "nihao", "text": "你好", "count": 12, "lastUsed": 1_782_050_000i64 },
            { "code": "women", "text": "我们", "count": 5, "lastUsed": 1_781_900_000i64 }
        ], "total": 2 }),
        "dict.search" => json!([
            { "code": "aaaa", "text": "工", "weight": 0 }
        ]),
        // 加词自动出码：与批量出码同一套占位规则（每字一个字母、BMP 外字符出不了码），
        // 而不是恒定的 "aaaa"——离线演示时词条一变编码就跟着变，才看得出"自动"这回事，
        // 也才有一条能离线复现的「出不了码」分支（见 `mock_word_code`）。
        "dict.encode" => json!(mock_word_code(
            params
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
        )),
        // 批量出码（纯词列表导入）。契约要害是 **codes 与 texts 同序等长**，
        // 出不了码回空串而非省略——客户端靠下标把码配回词。mock 用「BMP 外的生僻字
        // 出不了码」来复现真实的部分失败，好让 UI 那条跳过分支也能离线验证。
        "dict.encodeWords" => {
            let codes: Vec<Value> = params
                .get("texts")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|t| json!(mock_word_code(t.as_str().unwrap_or(""))))
                        .collect()
                })
                .unwrap_or_default();
            json!({ "codes": codes })
        }
        // 词库多段导入导出（sections 契约,形状对齐 core webdata.rs）。
        "dict.export" => {
            json!({ "content": "# WindInput 用户数据文件\nwind_dict:\n  version: 1\n  sections:\n    words:\n      columns: [code, text, weight, count]\n    shadow:\n      columns: [action, code, word, position, cand_id]\n\n--- !words\naaaa\t工\t0\t3\n\n--- !shadow\npin\tggkg\t王中\t0\t\n" })
        }
        // 目标是拼音方案时给出**带准入契约计数**的样例，否则维持原来的码表样例。
        // 两种形状都要能离线看到：那一行「音节分隔」的二选一只在拼音下出现，
        // 只留码表样例的话，mock 模式下它永远不显示、等于没法验。
        "dict.import" if mock_target_is_pinyin(params) => json!({ "sections": [
            { "key": "userWords", "added": 8, "updated": 0, "unchanged": 0, "skipped": 3,
              "rejected": 2, "boundaryFilled": 5, "boundaryAmbiguous": 1,
              "noBoundary": 0, "boundarySkipped": 1 }
        ] }),
        "dict.import" => json!({ "sections": [
            { "key": "userWords", "added": 1, "updated": 0, "unchanged": 0, "skipped": 0 },
            { "key": "shadow", "imported": 1, "skipped": 0 }
        ] }),
        "dict.previewImport" if mock_target_is_pinyin(params) => json!({
            "format": "rime", "compatible": true,
            "sourceSchema": "", "sourceEngine": "", "targetEngine": "pinyin",
            "sections": [
                { "key": "userWords", "count": 10, "willAdd": 8, "willUpdate": 0, "unchanged": 0,
                  "skipped": 2, "samples": ["你好"],
                  "rejected": 2, "rejectedSamples": ["wgkq 工", "aaaa 田"],
                  "boundaryFilled": 5, "boundaryAmbiguous": 1, "noBoundary": 0 }
            ] }),
        "dict.previewImport" => json!({ "format": "winddict", "compatible": true,
            "sourceSchema": "wubi86", "sourceEngine": "codetable", "targetEngine": "codetable",
            "sections": [
                { "key": "userWords", "count": 1, "willAdd": 1, "willUpdate": 0, "unchanged": 0, "skipped": 0, "samples": ["工"] },
                { "key": "shadow", "count": 1, "skipped": 0 }
            ] }),
        // 方案包导入导出（v2 契约：零层级条目名 + 可选 package 元信息）。
        "scheme.exportPackage" => {
            json!({ "path": "C:/tmp/demo.zip", "packed": ["demo.schema.toml"], "systemRefs": [], "missing": [] })
        }
        // 无 `title` / `description`：**无说明形**（旧包 / 生成端没填），确认对话框
        // 顶部整块不占位。有说明形由下面的信封样例覆盖。
        "scheme.previewImport" => json!({
            "package": {
                "package": { "app_version": "0.0.0", "platform": "windows", "created_at": "2026-07-13T00:00:00+08:00" },
                "schema": { "id": "demo", "version": "1.0" },
                "refs": { "system": [], "missing": [] }
            },
            "willAdd": ["demo.schema.toml"], "conflicts": [], "systemRefs": [], "missing": []
        }),
        "scheme.importPackage" => {
            json!({ "imported": ["demo.schema.toml"], "conflicts": [], "schemaIds": ["demo"] })
        }
        // 文本信封（§3.4）。响应形状与 zip 两法一致；这里刻意带上 configPatch，
        // 让「将修改的配置」区块（含 Map 条目显示键）离线也能核对。
        //
        // 说明（§2.3）刻意给**两级且取值不同**：顶层 title/description 是包级说明，
        // configPatch.info 是那段片段自己的说明。取一样的值就分不清 UI 是各显各的
        // 还是有一处串了源——两级说的不是同一件事，合并即错。
        "scheme.previewImportText" => {
            let mut v = json!({
                "package": {
                    "package": { "app_version": "0.0.0", "platform": "windows", "created_at": "2026-08-19T00:00:00+08:00" },
                    "schema": { "id": "kf", "version": "1.00.0" },
                    "refs": { "system": [], "missing": [] }
                },
                "title": "快符方案",
                "description": "分号引导的符号表，含常用标点与配对符号。\n随包附带一份按键功能配置。",
                "willAdd": ["kf.schema.toml", "flypy/12_kf.dict.yaml"], "conflicts": [], "systemRefs": [], "missing": [],
                "configPatch": {
                    "text": "[keys.key_actions]\nbacktick = \"quick_input\"\n",
                    "ok": true,
                    "entries": [
                        { "key": "keys.key_actions", "mapEntry": "backtick", "next": "quick_input" }
                    ],
                    "info": {
                        "title": "快符引导键",
                        "description": "把反引号设为快捷输入引导键。"
                    }
                }
            });
            if mock_wants_long_info(params) {
                v["configPatch"]["info"] = mock_limit_info();
                v["title"] = mock_limit_info()["title"].clone();
                v["description"] = mock_limit_info()["description"].clone();
            }
            v
        }
        "scheme.importText" => {
            json!({ "imported": ["kf.schema.toml", "flypy/12_kf.dict.yaml"], "conflicts": [], "schemaIds": ["kf"] })
        }
        // 删除方案:级联清理结果(deleted/keptShared 供 toast 汇总)。
        "schema.delete" => json!({
            "ok": true,
            "deleted": ["demo.schema.toml", "demo/d.dict.yaml"],
            "keptShared": [],
            "schemaIds": ["demo"]
        }),
        // 配置片段导入（core 契约：previewPatch 只读逐条校验，current 缺席=未设置；
        // applyPatch 全部合法才落盘，逐键错误时整体为 RPC error）。
        // Map 键（key_actions）刻意给一条：预览逐**条目**报告、显示键为
        // `父键[条目名]`，且 written 回传的是合并后的整表（§2.2 / §2.3）。
        // `info` = 片段 `[package]` 段的说明（§2.3）；两字段都缺省时 core 不发它，
        // 那一形由 `scheme.previewImport`（纯方案包，无说明）覆盖。
        "config.previewPatch" => {
            let mut v = json!({
                "ok": true,
                "entries": [
                    { "key": "ui.candidate.per_page", "current": 5, "next": 9 },
                    { "key": "input.initial_mode", "next": "chinese" },
                    { "key": "keys.key_actions", "mapEntry": "backtick", "next": "quick_input" }
                ],
                "info": {
                    "title": "九候选 + 默认中文",
                    "description": "把候选栏调成每页九个，并把启动模式设为中文。\n反引号设为快捷输入引导键。"
                }
            });
            if mock_wants_long_info(params) {
                v["info"] = mock_limit_info();
            }
            v
        }
        // written = 落盘后的最终键值：Map 父键带的是合并结果整表（片段只写了
        // backtick，semicolon 是用户原有条目），客户端拼不出来，必须由 core 回传。
        "config.applyPatch" => json!({
            "ok": true, "applied": 3, "needsRestart": false,
            "written": [
                { "key": "ui.candidate.per_page", "value": 9 },
                { "key": "input.initial_mode", "value": "chinese" },
                { "key": "keys.key_actions",
                  "value": { "semicolon": "temp_pinyin", "backtick": "quick_input" } }
            ]
        }),
        // 整机备份（P4 契约）。
        "backup.create" | "backup.inspect" => json!({
            "path": "C:/tmp/backup.zip",
            "manifest": { "format": "windinput-bundle", "kind": "backup", "spec_version": 1,
                          "app_version": "0.0.0", "platform": "windows", "created_at": "2026-07-13T00:00:00+08:00",
                          "contents": [
                              { "type": "config", "path": "config/config.toml", "meta": null },
                              { "type": "dict", "path": "userdata/user_words/wb.wdict", "meta": { "schema": "wb" } },
                              { "type": "phrase", "path": "userdata/phrases.wdict", "meta": null }
                          ] }
        }),
        "backup.restore" => json!({
            "restored": ["config/config.toml", "userdata/user_words/wb.wdict"],
            "conflicts": [],
            "schemasTouched": ["wb"]
        }),
        "shadow.addRule" | "shadow.removeRule" => json!({ "ok": true }),
        m if m.starts_with("compat.") => crate::pages::compat::mock::reply(m, params),
        _ => Value::Null,
    }
}
