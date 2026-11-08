# tiny_http 0.12.0 本地补丁

来源：crates.io `tiny_http-0.12.0` 原样拷入，经根 `Cargo.toml` 的 `[patch.crates-io]` 接管。
只拷了 `src/`、许可证与 README；`benches/`、`tests/`、`examples/` 与 dev-dependencies 已删
（不参与本仓构建，留着会把它们的依赖拉进 Cargo.lock）。

为什么要补：主题编辑器连接（`src/theme_editor_server.rs`）是本机 HTTP 服务，网页能往这里发
「简单请求」而不经预检。原实现有两处会被一个请求拖垮：

1. `src/util/equal_reader.rs` `EqualReader::drop`：请求体没读完就回响应时，按剩余的**声明**长度
   一次性 `vec![0; remaining]` 来丢弃余量。`Content-Length` 写得离谱（如 2^60）时分配失败，
   整个设置进程 abort。
   - 改为固定 8 KiB 缓冲，最多再丢弃 1 MiB；超出预算不再读（连接上残留的字节会让下一次
     请求解析失败，连接随之关闭）。
2. 连接没有读写超时：只发请求头、不发请求体（或不收响应）的客户端会无限期占住工作线程。
   - `src/lib.rs`：接受连接时设读写超时，缺省 `DEFAULT_SOCKET_TIMEOUT` = 30 秒；
     新增 `Server::http_with_socket_timeout(addr, timeout)`（测试用短超时）。
   - `src/connection.rs`：`Connection::set_timeouts`。

`Cargo.toml` 末尾关掉了上游原有的 unused / clippy 告警（path 依赖不走 cap-lints）；`rustfmt.toml`
关掉了格式化（上游代码原样保留，本仓提交钩子按 2024 风格查格式会误拦）。

所有改动处都标了 `[wind-setting 补丁]`。升级 tiny_http 时：先看上游是否已修这两处，修了就删掉
本目录与 `[patch.crates-io]`；没修就把这些改动搬到新版本上，并跑
`theme_editor_server::route_tests`（含巨大 Content-Length、慢速请求体两条用例）。
