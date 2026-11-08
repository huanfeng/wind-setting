# tiny_http 0.12.0 本地补丁

来源：crates.io `tiny_http-0.12.0` 原样拷入，经根 `Cargo.toml` 的 `[patch.crates-io]` 接管。
只拷了 `src/`、许可证与 README；`benches/`、`tests/`、`examples/` 与 dev-dependencies 已删
（不参与本仓构建，留着会把它们的依赖拉进 Cargo.lock）。

为什么要补：主题编辑器连接（`src/theme_editor_server.rs`）是本机 HTTP 服务，网页能往这里发
「简单请求」而不经预检。原实现有几处会被一个请求利用：

1. `src/util/equal_reader.rs` `EqualReader::drop`：请求体没读完就回响应时，按剩余的**声明**长度
   一次性 `vec![0; remaining]` 来丢弃余量。`Content-Length` 写得离谱（如 2^60）时分配失败，
   整个设置进程 abort。
   - 改为固定 8 KiB 缓冲，最多再丢弃 1 MiB、最多再读 2 秒（滴灌每次读都不超时，靠总时限
     收手）；超出就不再读。截止只在两次读之间查，最坏约 2 秒 + 一次读超时 30 秒 = 32 秒。
2. 连接没有读写超时：只发请求头、不发请求体（或不收响应）的客户端会无限期占住工作线程。
   - `src/lib.rs`：接受连接时设读写超时，缺省 `DEFAULT_SOCKET_TIMEOUT` = 30 秒；
     新增 `Server::http_with_socket_timeout(addr, timeout)`（测试用短超时）。
   - `src/connection.rs`：`Connection::set_timeouts`。
3. 请求走私：丢弃预算用完后连接上还留着没读的请求体，原实现会在同一条连接上把它当成下一个
   请求解析。网页 `fetch(no-cors)` 发「1 MiB 填充 + 伪造 Host / Origin 的请求」，后半截就绕过了
   服务端的 Host / Origin 校验。分块请求体（不经丢弃逻辑）同理。
   - `src/client.rs` `ClientConnection::next`：返回请求前置 `no_more_requests = true`，
     一条连接只处理一个请求；
   - `src/response.rs` `raw_print`：非 upgrade、状态码 ≥ 200 的响应带 `Connection: close`
     （100 Continue 是中间响应，不带）；
   - `src/client.rs`：回 505（不支持的 HTTP 版本）后也关连接，原为 `continue` 接着解析。版本
     检查顺带提前到建 Request 之前（`ReadError::UnsupportedVersion`）：原实现在 Request 已占住
     第一个响应写入器之后才写 505，505 永远排不上，连接挂死。检查发生在读请求头之前，回 505 时剩余请求头
     不再读，直接关连接（之后不会再解析这条连接上的任何字节，所以不读也无妨）。
4. 请求头无上限：原实现读一行一直读到换行，不带换行的超长行会把内存吃光。
   - `src/client.rs` `read_next_line`：单行 16 KiB 上限；`read`：请求头最多 100 条。超了回 400
     并关连接。

`Cargo.toml` 末尾关掉了上游原有的 unused / clippy 告警（path 依赖不走 cap-lints）；`rustfmt.toml`
关掉了格式化（上游代码原样保留，本仓提交钩子按 2024 风格查格式会误拦）。

所有改动处都标了 `[wind-setting 补丁]`。升级 tiny_http 时：先看上游是否已修这几处，修了就删掉
本目录与 `[patch.crates-io]`；没修就把这些改动搬到新版本上，并跑
`theme_editor_server::route_tests`（巨大 Content-Length、慢速 / 滴灌请求体、声明长度与分块两种
请求走私、超长请求头都有用例）。
