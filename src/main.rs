// 发布构建走 GUI 子系统（不弹控制台窗口）；**测试构建不走**——`cargo test` 把本 crate 编成
// 一个 test harness，GUI 子系统下它没有自己的控制台，在 wine 里跑（`dev.sh st`，交叉到
// Windows 目标后由 wine 执行）就看不到测试输出。真 Windows 上 cargo 用管道捕获 stdout 故
// 一直没暴露这个问题，Linux 侧一跑就撞上。加 `not(test)` 两边都对。
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! wind_setting — 清风输入法原生设置应用（基于 windui）。
//!
//! 通过 JSON-RPC over named pipe 与核心服务通信，不改动 core 接口。
//! 无边框窗口 + 自定义标题栏，7 Tab 清单驱动（manifest 分组），运行时明暗换肤。

mod accent;
mod add_word_window;
mod auto_encode;
mod by_layout;
mod capabilities;
mod cli;
mod compat_window;
mod config;
mod dialogs;
mod dict_window;
mod dota2_compat;
mod file_assoc;
mod icons;
mod key_conflict;
mod logger;
mod manifest;
mod memstat;
mod mix_fields;
mod mix_members;
mod mode;
mod open_external;
mod pages;
mod protocol;
mod rpc;
mod schema_url_import;
mod search;
#[allow(dead_code)]
mod setting_config;
mod shell;
mod shortcuts;
mod state;
mod subnav;
mod theme_def;
#[allow(dead_code)]
mod theme_editor_server;
mod theme_follow;
#[allow(dead_code)]
mod theme_import;
mod tsf_log;
#[allow(dead_code)]
mod update;
mod user_dir;
mod widgets;
mod wizard;
#[cfg(target_os = "linux")]
mod xdg_assoc;

/// 计数分配器：仅 `--features memstat` 时接管全局分配，用于把内存占用拆到具体构建阶段。
#[cfg(feature = "memstat")]
#[global_allocator]
static ALLOC: memstat::Counting = memstat::Counting;

use std::cell::RefCell;
use std::rc::Rc;

use windui::core::EventCtx;
use windui::prelude::*;

/// 没有 `EventCtx` 的回调（`single_instance` 的 argv 处理）送往 UI 线程的请求。
///
/// 之所以要绕这一道：开窗请求只在副作用被采纳的时机才算数（控件事件回调、
/// `App::channel` 的 `on_message`、`on_interval`），而 `single_instance` 的回调虽然也在
/// UI 线程，却只收 argv、拿不到 ctx。经 channel 转一手，回调那侧就有 ctx 了。
pub enum UiRequest {
    /// 打开词库管理窗（实际开不开取决于 `take_dict_window_request`）。
    OpenDictWindow,
    /// 打开应用兼容窗（二次实例带 `--page compat`）。
    OpenCompatWindow,
    /// 重新开出主窗（主窗已关、词库窗还开着时点「设置」）。
    OpenMainWindow,
}

/// 设置窗尺寸。`App::new` 与主窗重建两条路共用一份，免得各写各的、改一边漏一边。
///
/// ★ 宽度随二级导航一起从 820 抬到 950。左侧从「184 单栏」变成「140 + 1 + 168 + 1
/// 三栏」，多吃掉 125px；不抬宽度的话内容区只剩约 480，方案页工具栏那排按钮会**再一次**
/// 被挤出可视区（`pages::scheme_page` 里已为同一个症状把工具栏拆过两行）。950 - 310 = 640，
/// 与改动前的 820 - 185 = 635 基本持平，等于「内容区宽度不变，导航多了一栏」。
pub(crate) const MAIN_W: i32 = 950;
pub(crate) const MAIN_H: i32 = 640;
/// 最小宽度同步从 560 抬到 700：三栏导航占 310，560 下内容区只剩 250，
/// 设置行的标题、说明与右侧控件挤在一起，等于这个下限处的界面不可用。
pub(crate) const MAIN_MIN_W: i32 = 700;
pub(crate) const MAIN_MIN_H: i32 = 440;

/// 词库窗尺寸。**曾经与设置窗共用一份**，二级导航把设置窗撑宽之后拆开——
/// 那 125px 是拿来放二级导航栏的，词库窗根本没有那一栏，跟着变宽只是白占屏幕。
/// 故这里保留原来的 820 × 640 / 最小 560 × 440。
///
/// 两条路要用它：[`dict_window::open`]（从设置窗开词库），以及 `--page dict` 冷启动时
/// 让 `App` 那个窗口**直接承载词库**的分叉（见 `main` 里的 `dict_as_main`）。漏掉后者
/// 的症状是「托盘点词库管理开出来的窗比从设置里开的宽一圈」。
pub(crate) const DICT_W: i32 = 820;
pub(crate) const DICT_H: i32 = 640;
pub(crate) const DICT_MIN_W: i32 = 560;
pub(crate) const DICT_MIN_H: i32 = 440;

/// 应用兼容窗尺寸：三栏（分类 184 + 进程列表 270 + 配置信息），配置信息一行要放下
/// 标题、说明与控件，所以比词库窗宽一圈。
pub(crate) const COMPAT_W: i32 = 1040;
pub(crate) const COMPAT_H: i32 = 680;
pub(crate) const COMPAT_MIN_W: i32 = 900;
pub(crate) const COMPAT_MIN_H: i32 = 480;

/// 设置主窗的单例键。窗口在的时候，别处 `open_window(Window::single(本键))` 会把它
/// 激活到前台而不是再开一个——[`open_main_window`] 与词库窗的两个入口都靠这条。
pub(crate) const MAIN_SINGLE_KEY: &str = "main";

/// `App` 那个窗口这一趟要登记哪个单例键。
///
/// **按它承载的界面选，不能一律 `MAIN_SINGLE_KEY`**：`--page dict` 冷启动时 `App` 建的
/// 是词库窗（见下面 `dict_as_main`），此时若把它登记成「main」，之后任何一次
/// [`open_main_window`] 都会命中它、被平台当成"设置窗已经开着"，于是只把这个词库窗
/// 激活到前台——用户点「设置」，跳出来的是他已经在看的词库。
fn main_window_single_key(dict_as_main: bool) -> &'static str {
    if dict_as_main {
        dict_window::SINGLE_KEY
    } else {
        MAIN_SINGLE_KEY
    }
}

/// 重新开出主窗。
///
/// 主窗关掉后进程并不退出——词库窗还开着的话它照常活着（windui 是「关掉最后一个窗口
/// 才退出」）。此时用户再点「设置」，单实例监听（独立的 message-only 窗口，不随主窗死）
/// 会收到 argv，但 windui 只会去 activate 那个已经不存在的主窗，什么都不会发生。
/// 于是这里重新建一个。
///
/// 重建出来的是子窗，与 `App::run` 建的那个有一处**实质差别**：它不能注册 `App::channel`
/// （通道只在建窗前可注册）。但通道本身挂在 App 级、并没有随主窗一起死，原先注册的三条
/// （数据加载 / core 事件 / 升级）照常有人排空——重建后仍然收得到。
///
/// **主窗还开着时调用本函数是安全的**：`single(MAIN_SINGLE_KEY)` 此时命中既有窗口，
/// 平台把它激活到前台（最小化的会还原），本次请求被丢弃，不会建出第二个。首个主窗
/// 也登记了同一个键（`App::single_window`，见 [`main_window_single_key`]）——那正是
/// 「把主窗拉到前台」唯一可走的路：`EventCtx` 没有"激活某个窗口"的原语，只有平台层
/// 在单例判定命中时代做。
///
/// 故调用方**不需要**先查 `main_window_closed`：那个标志只用来分辨二次实例该 activate
/// 还是重建，不是本函数的前置条件。
/// `App` 那个窗口是不是设置主窗（决定 App 级诊断定时器挂不挂）。
///
/// 词库 / 兼容窗当首窗时不挂：之后开出的设置主窗由 [`open_main_window`] 自挂一份，
/// 两份同时跑就是每 2 秒取两次。
fn app_hosts_settings_shell(dict_as_main: bool, compat_as_main: bool) -> bool {
    !dict_as_main && !compat_as_main
}

pub(crate) fn open_main_window(ctx: &mut EventCtx, app: &Rc<RefCell<state::AppState>>) {
    // 建窗前必然已注入（见 main 里 `set_theme_handle`）。真取不到就只能放弃重建：
    // 没有句柄，这个窗口的换肤（明暗、主题色）就与其余窗口脱钩。
    if app.borrow().theme_handle.borrow().is_none() {
        tracing::error!("主窗重建失败：换肤句柄缺失");
        return;
    }
    let st = app.clone();
    let guard = app.clone();
    let tick = app.clone();
    // 重建即视为主窗回来了：这一步要赶在 content 求值之前，否则窗口已经在了、
    // 标志还写着"已关闭"，下一次二次实例会再重建一个。
    app.borrow().main_window_closed.set(false);
    ctx.open_window(
        Window::new("清风输入法设置", MAIN_W, MAIN_H)
            .single(MAIN_SINGLE_KEY)
            .frameless(true)
            .centered(true)
            .min_size(MAIN_MIN_W, MAIN_MIN_H)
            // 同 `main` 里那份：App 级定时器挂在首个主窗上、随它关掉了，重建的要自己挂。
            .on_interval(pages::memory::TICK, move |_ctx| {
                pages::memory::on_tick(&tick)
            })
            // 与 `App::on_close_request` 同一套守卫：有未保存改动就拦下弹确认框。
            // 子窗的拦截器必须自己挂——平台在 WM_CLOSE 里同步问的是「**这个**窗口能不能关」，
            // 主窗那份不代管子窗。
            .on_close_request(move |_ctx| {
                let Ok(s) = guard.try_borrow() else {
                    return true;
                };
                // 清单只算一次，它自己就是守卫的判据：空 = 没有未保存改动。
                let unsaved = s.unsaved_items();
                if unsaved.is_empty() {
                    s.mark_main_window_closed();
                    return true;
                }
                s.prime_exit_confirm(&unsaved);
                false
            })
            .content(move || shell::build(&st)),
    );
}

/// 把 `--page` / `--tab` 解出的目标下标分派到「切页」或「开词库窗」。
///
/// `dict` 已不是一个页：词库整体迁进了独立窗，主窗那一格只剩个永不显示的占位
/// （见 `pages::dict_page`），`tab.set(dict_idx)` 只会得到一屏空白。
///
/// `notify` 透传给 [`state::AppState::request_dict_window`]：冷启动传 `false`（等
/// `apply_loaded` 注入方案后再开），二次实例传 `true`（数据早已就绪，立刻推一帧）。
///
/// 冷启动与二次实例共用此入口 —— 两处各写一份 if 的话，改一边漏一边，而漏掉的那条
/// 路径的症状是「点了词库管理，设置窗跳出来却是一片空白」。
fn route_target(st: &state::AppState, idx: usize, notify: bool) {
    if Some(idx) == cli::page_to_index("dict") {
        st.request_dict_window(notify);
    } else {
        st.tab.set(idx);
    }
}

/// 同步拉取数据并 apply；连接不存在时 fast-fail 立即返回。重连复用此入口。
///
/// 设置操作低频，故直接在调用线程同步 fetch（无后台线程）；
/// 若重连后仍失败，`apply_loaded` 会再次把 `conn_failed` 置 true，对话框保持。
pub fn main_reconnect(app: &Rc<RefCell<state::AppState>>) {
    let data = state::LoadedState::fetch();
    let connected = data.connected;
    app.borrow_mut().apply_loaded(data);
    // 启动时 core 没起来的用户，重连成功这一刻才是首启向导该弹的时候。
    wizard::maybe_auto_open(app, connected);
}

/// 单实例 app_id（含变体后缀，dev/release 互不干扰）。主窗口与加词小窗是两个独立单例，
/// 各占一个 id —— 加词小窗开着时点「设置」，两者互不顶替。
///
/// `claim_instance` 与 `App::single_instance` 必须用同一个 id，故收拢在此。
fn single_instance_app_id(add_word: bool) -> String {
    let suffix = mode::pipe_suffix();
    if add_word {
        format!("wind_setting_addword{suffix}")
    } else {
        format!("wind_setting{suffix}")
    }
}

fn main() {
    // mode::init 必须最先执行：logger 的目录路径和 RPC 管道名均依赖它。
    let args: Vec<String> = std::env::args().collect();
    mode::init(&args);

    // 单实例闸门：设置程序已开着时，本进程的使命就只是把 argv（深链页、协议 URL）递给
    // 首实例，然后死掉。故仲裁要赶在任何启动动作之前 —— windui 在 App::run 里也会仲裁，
    // 但那时这个注定要退场的进程已经轮转过日志、发了七八个 RPC、订阅了 core 事件通道，
    // theme_editor.auto_start 开着的话还会去抢端口。这些副作用全落在首实例头上。
    let add_word = cli::parse_add_word(&args);
    if windui::claim_instance(&single_instance_app_id(add_word.is_some()))
        == windui::InstanceRole::Handoff
    {
        return;
    }

    // 采样日志在单例闸门**之后**才清空：二次实例（切页深链）也会走到 main，
    // 放在闸门前会把首实例的采样记录一次次抹掉。
    memstat::reset_log();
    memstat::mark("[进程起点]");

    logger::init();
    memstat::mark("[日志就绪]");
    // 协议与 .wpkg 关联的自愈各有两道门：便携版不碰宿主机注册表，用户手动取消过
    // 注册的也不再写回去（两条关联各记各的意愿，取消其一不影响另一条）。
    // 单读一次配置而不等 AppState 建好 —— 自愈要赶在窗口出现前完成，此刻 AppState
    // 还没开始拉 core 数据。判定本身是纯函数，见 protocol::should_self_heal_on_startup。
    {
        let sc = setting_config::load().config;
        let portable = mode::is_portable();
        if protocol::should_self_heal_on_startup(portable, sc.protocol.self_heal) {
            protocol::self_heal_registration();
        }
        if protocol::should_self_heal_on_startup(portable, sc.file_assoc.self_heal) {
            file_assoc::self_heal_registration();
        }
    }

    // 快捷加词小窗：独立单例、紧凑窗，复用词库加词对话框；不落入常规设置界面路径。
    if let Some(params) = add_word {
        add_word_window::run(params);
        return;
    }

    // 快速初始化：仅读系统主题（~86µs），不阻塞任何 RPC，UI 立即可显示。
    let app_state = Rc::new(RefCell::new(state::AppState::empty()));
    memstat::mark("[AppState 建好]");
    {
        let st = app_state.borrow();
        if args.iter().any(|a| a == "--light") {
            st.dark.set(false);
        } else if args.iter().any(|a| a == "--dark") {
            st.dark.set(true);
        }
        // 初始页：--page <id> / --page=<id> / 快捷 --<id> / --tab <n>。
        // `--page dict` 走开窗而非切页，且冷启动不立刻推帧（等 apply_loaded），见 route_target。
        if let Some(idx) = cli::parse_target(&args) {
            route_target(&st, idx, false);
        }
        // 词库页深链（--schema/--type）：此刻方案列表尚未就绪，故只挂起，
        // 待 apply_loaded 注入方案后再定位（见 AppState::route_dict_target）。
        if let Some(t) = cli::parse_dict_target(&args) {
            st.route_dict_target(t);
        }
        // 冷启动协议 URL：解析并路由（切相关页 + 占位提示）。
        if let Some(raw) = cli::parse_protocol(&args) {
            drop(st);
            route_protocol(&raw, &app_state);
        }
    }
    if app_state.borrow().setting_config.theme_editor.auto_start {
        app_state.borrow().start_theme_editor_server(true);
    }
    if let Some(w) = app_state.borrow().setting_config_warning.clone() {
        app_state.borrow().feedback.set(Some((ToastKind::Info, w)));
    }
    let start_dark = app_state.borrow().dark.get();
    // 跟随输入法主题色时，建窗前先取一次色：否则窗口先按默认蓝亮出来、数据到了再变色，
    // 开窗就闪一下。限时取，取不到先用默认色（见 `prefetch_accent`）。
    app_state.borrow().prefetch_accent();

    // 渲染：默认 `Renderer::Auto`（GPU 优先，远程会话 / 无可用 GPU 时由 windui 自动回退
    // 软渲染，不 panic）。命令行 --soft / --no-accel 强制软渲染。
    //
    // 显式写 Auto 而非省略：windui 的默认值是 `Software`（其 GPU 路径的验证仍在补齐），
    // 而本应用一直跑在 GPU 加速上——省略参数会静默降级成软渲染。
    let soft_render = args.iter().any(|a| a == "--soft" || a == "--no-accel");
    let renderer = if soft_render {
        Renderer::Software
    } else {
        Renderer::Auto
    };
    tracing::info!(
        "渲染模式: {}",
        if soft_render {
            "软件 (tiny-skia)"
        } else {
            "GPU 优先（不可用时自动回退软渲染）"
        }
    );

    // `--page dict` 冷启动（托盘/开始菜单的「词库管理」）让 `App` 的那个窗口**直接承载
    // 词库**，不再建设置窗。此前是照建设置窗、再往上叠一个词库窗，于是点一次「词库管理」
    // 冒出两个窗口——而用户要的就只是词库。
    //
    // 只能在这里分叉：`App::run` 建的窗口是无条件出现的，没有"建好但不显示"这条路
    // （`start_hidden` 要求有托盘或全局热键把它唤回来，本应用两样都没有）。
    let dict_as_main = app_state.borrow().take_dict_window_request();
    if dict_as_main {
        let st = app_state.borrow();
        st.set_dict_is_main(true);
        // 设置窗这一趟压根没建过，故按"已关闭"记账：之后点「设置」才会走重建那条路。
        st.mark_main_window_closed();
    }
    // `--page compat` 同理：让 `App` 的窗口直接承载应用兼容窗，不再多叠一个设置窗。
    let compat_as_main = !dict_as_main && cli::parse_compat_request(&args);
    if compat_as_main {
        app_state.borrow().mark_main_window_closed();
    }
    // 标题与尺寸都跟着这一趟承载的界面走：设置窗要多留一栏二级导航的宽度，词库窗不要
    // （见 [`DICT_W`]）。此前两者同尺寸，只有标题在这里分叉。
    let (title, (win_w, win_h), (win_min_w, win_min_h)) = if compat_as_main {
        (
            compat_window::TITLE,
            (COMPAT_W, COMPAT_H),
            (COMPAT_MIN_W, COMPAT_MIN_H),
        )
    } else if dict_as_main {
        (
            dict_window::TITLE,
            (DICT_W, DICT_H),
            (DICT_MIN_W, DICT_MIN_H),
        )
    } else {
        ("清风输入法设置", (MAIN_W, MAIN_H), (MAIN_MIN_W, MAIN_MIN_H))
    };
    memstat::mark("[App::new 前]");
    let app = App::new(title, win_w, win_h)
        // 单例键要跟着这一趟承载的界面走（见 `main_window_single_key`）。
        .single_window(if compat_as_main {
            compat_window::SINGLE_KEY
        } else {
            main_window_single_key(dict_as_main)
        })
        .frameless()
        .centered()
        .renderer(renderer)
        .min_size(win_min_w, win_min_h)
        .theme(app_state.borrow().ui_theme());
    // 窗口级快捷键。绑定表与分派都在 `shortcuts`——加一条快捷键在那里加一行即可。
    //
    // ⚠ Escape 这条**必须**有人管：TextInput 不处理 Escape，未消费的 Escape 会被宿主
    // 当成「关窗兜底」——搜索到一半按 Esc 就把整个设置窗关掉了。`on_shortcut` 排在
    // Tab 焦点导航与 Escape 关窗**之前**，正是为覆盖这类默认行为；而浮层没开时
    // `dispatch` 会返回「未处理」，关窗兜底照常生效。
    let shortcut_state = app_state.clone();
    let mut app = app.on_shortcut(move |ctx, ev| {
        let out = shortcuts::dispatch(&ev, &shortcut_state.borrow());
        if out.focus_main_input {
            ctx.focus_main_input();
        }
        if out.request_close {
            ctx.request_close();
        }
        // 提交词库页的加词对话框。先把 `DictManagerState` clone 出来（它是一组信号 +
        // `Rc`，clone 很便宜，且不回指 `AppState`），提交期间不持有 `AppState` 的
        // 借用——那期间要发 RPC、写信号、触发重排，让一个 `RefCell` 借用横跨这些
        // 是没必要冒的险。
        if let Some(kind) = out.submit_dict {
            let mgr = shortcut_state.borrow().dict_mgr.clone();
            match kind {
                shortcuts::DictSubmit::Word => mgr.submit_word(),
                shortcuts::DictSubmit::Shadow => mgr.submit_shadow(),
            }
        }
        out.handled
    });
    let th = app.theme_handle();
    // 存一份供重建主窗用：`App::theme_handle()` 只在建窗前拿得到，而重建发生在运行期。
    *app_state.borrow().theme_handle.borrow_mut() = Some(th.clone());
    // 记下建窗用的那份（明暗, 强调色），之后同值就不再整窗换肤。
    app_state.borrow().apply_ui_theme();

    // 导入意图（`--page import` 深链 / `.wpkg` 文件关联双击）统一经此通道到 UI 线程：
    // 读剪贴板要 `EventCtx`，只有 channel 回调拿得到；channel 靠出帧排空，冷启动在
    // 首帧执行，正好也保证了 UI 树已就绪。冷启动与二次唤起同一条路，行为不会分叉。
    let import_action_state = app_state.clone();
    let import_tx = app.channel::<ImportAction>(move |ctx, act| {
        run_import_action(ctx, act, &import_action_state);
    });

    // 冷启动的导入意图：解析在此，执行在首帧（见上）。
    if cli::parse_import_request(&args) {
        let _ = import_tx.send(ImportAction::ClipboardDialog);
    } else if let Some(p) = cli::parse_import_file(&args) {
        let _ = import_tx.send(ImportAction::File(p));
    }

    // 单实例 + 二次运行激活/切页/协议/导入。
    let app_id = single_instance_app_id(false);
    let single_instance_state = app_state.clone();
    let mut app = app.single_instance(app_id, move |argv| {
        // 深链失效（点了"词库管理"却停在原页）时，这条是唯一能分辨「argv 没送到」
        // 与「送到了但解析不出页」的证据。两种成因的修法完全不同，值一条 info。
        tracing::info!("二次实例 argv: {:?}", argv.get(1..).unwrap_or(&[]));
        if let Some(raw) = cli::parse_protocol(&argv) {
            route_protocol(&raw, &single_instance_state);
        } else if cli::parse_import_request(&argv) {
            let _ = import_tx.send(ImportAction::ClipboardDialog);
        } else if let Some(p) = cli::parse_import_file(&argv) {
            let _ = import_tx.send(ImportAction::File(p));
        } else if cli::parse_compat_request(&argv) {
            // 应用兼容不是主窗的一页，走开窗；单例键已存在时（含它承载在 App 窗口里）只是激活。
            single_instance_state.borrow().request_compat_window();
        } else if let Some(idx) = cli::parse_target(&argv) {
            let st = single_instance_state.borrow();
            // 已运行的实例通常方案列表早已就绪，深链会立即生效；若恰好撞上启动
            // 头几十毫秒数据还没回来，route_dict_target 会自动改为挂起。
            if let Some(t) = cli::parse_dict_target(&argv) {
                st.route_dict_target(t);
            }
            // notify = true：本回调没有 ctx、开不了窗（见 `UiRequest`），要经通道推一帧。
            // 词库窗若已开着，`Window::single` 会把它激活到前台，深链参数照样生效。
            route_target(&st, idx, true);
        }

        // 主窗已经关掉（词库窗还开着、进程因此活着）时，windui 只会去 activate 一个
        // 不存在的窗口，用户看到的是「点了设置没反应」。这里补一次重建。
        //
        // ⚠️ 必须在上面那串 if-else **之外**：最常见的一次二次启动就是从托盘或开始菜单
        // 点「设置」，那次 argv 一个参数都没有，上面每个分支都不匹配。把重建挂在
        // `parse_target` 分支里，等于给它加了个"这次得带参数"的莫须有前提——而重建与
        // argv 想干什么无关，它是**所有**后续动作的前置条件。
        //
        // 主窗还在的话这里什么都不做：单实例监听自己会把它拉到前台。
        //
        // 要的是应用兼容窗时不重建：用户没要设置窗，多冒一个只会添乱。
        let st = single_instance_state.borrow();
        if st.main_window_closed.get() && !cli::parse_compat_request(&argv) {
            st.request_main_window();
        }
    });

    // 注册后台→UI 通道：数据就绪后在 UI 线程回调，更新 Signal 刷新界面。
    let app_state_cb = app_state.clone();
    let tx = app.channel::<state::LoadedState>(move |ctx, data| {
        let connected = data.connected;
        app_state_cb.borrow_mut().apply_loaded(data);
        memstat::mark("[apply_loaded 后]");
        // 首启自动弹设置向导：数据就绪后才能按当前配置预填。
        wizard::maybe_auto_open(&app_state_cb, connected);
        // 冷启动 `--page dict` 的落点：方案列表刚刚注入，此刻开窗左栏才有内容、
        // `--schema` 也才解析得出域下标。channel 回调的副作用走
        // `apply_dispatch_effects`，`open_window` 在这条路上是被采纳的。
        if app_state_cb.borrow().take_dict_window_request() {
            dict_window::open(ctx, &app_state_cb);
        }
    });

    // 后台线程并行执行全部 RPC（fast-fail，管道不存在时几乎立即返回）。
    std::thread::spawn(move || {
        let data = state::LoadedState::fetch();
        if tx.send(data).is_err() {
            tracing::warn!("窗口已关闭，后台数据发送取消");
        }
    });

    // core 事件订阅：控制通道是请求-响应的，问不到 core 自发的状态变更（用户用快捷键
    // 或托盘切了方案等）。这里订阅单向事件管道，让界面跟着变——否则「当前方案」这类
    // 显示只在打开页面那一刻正确，之后会变成看起来权威的错误信息。
    // 监听线程内阻塞读 + 断线重连，故与升级通道一样经 channel 回 UI 线程写 Signal。
    let events_state = app_state.clone();
    let events_tx = app.channel::<rpc::CoreEvent>(move |ctx, ev| {
        events_state.borrow_mut().on_core_event(ctx, &ev);
    });
    std::thread::spawn(move || {
        rpc::listen_events(|ev| {
            // 发送失败 = 窗口已关闭，监听线程随进程退出即可。
            let _ = events_tx.send(ev);
        });
    });

    // 升级通道：下载线程不能直接写 Signal（Signal 是线程局部 arena 句柄，非 Send），
    // 故所有升级进度/结果都经此 channel 回到 UI 线程。channel 只能在 App 构建期注册，
    // 因此这里建好后把 Sender 存进 AppState 供各按钮取用。
    let update_state = app_state.clone();
    let update_tx = app.channel::<update::UpdateMsg>(move |_ctx, msg| {
        update_state.borrow().on_update_msg(msg);
    });
    app_state.borrow().set_update_tx(update_tx);

    // 内存诊断通道：`system.memoryStats` 在后台线程取（core 不在线时要等管道超时），
    // 结果经此回 UI 线程写摘要 / 对话框（见 `pages::memory`）。
    let memory_state = app_state.clone();
    let memory_tx = app.channel::<pages::memory::MemoryReply>(move |_ctx, r| {
        memory_state.borrow().memory.on_reply(r);
    });
    app_state.borrow().memory.set_tx(memory_tx);

    // UI 请求通道：`single_instance` 的 argv 回调没有 ctx，开不了窗（见 `UiRequest`），
    // 由它把请求转到这里 —— channel 的 on_message 收 `&mut EventCtx`，副作用被采纳。
    let ui_state = app_state.clone();
    let ui_tx = app.channel::<UiRequest>(move |ctx, req| match req {
        UiRequest::OpenDictWindow => {
            if ui_state.borrow().take_dict_window_request() {
                dict_window::open(ctx, &ui_state);
            }
        }
        UiRequest::OpenCompatWindow => compat_window::open(ctx, &ui_state),
        UiRequest::OpenMainWindow => {
            if ui_state.borrow().take_main_window_request() {
                open_main_window(ctx, &ui_state);
            }
        }
    });
    app_state.borrow().set_ui_tx(ui_tx);

    // 启动静默检查更新：三重门控（已同意联网 + 未关自动检查 + 距上次超 24h）。
    // 无更新或失败都不打扰用户，只有确实有新版本且未被「跳过此版本」屏蔽才弹对话框。
    {
        let now = chrono::Local::now().timestamp();
        let cfg = app_state.borrow().setting_config.update.clone();
        let forced = update::force_check_requested(&args);
        // 强制检查仍受 network_consent 门控 —— 调试后门不该越过联网同意
        let gate = if !update::update_check_supported() {
            // 没有元数据可查的平台不发请求。门控放在这里而不是 should_auto_check 里边 ——
            // 那是个纯函数，三重门控的语义与平台无关，掺进平台判断会让它的单测在别的平台
            // 上集体失效。Linux 能查（只提示、不自动安装），见 update_check_supported。
            false
        } else if forced {
            cfg.network_consent
        } else {
            update::should_auto_check(cfg.network_consent, cfg.auto_check, cfg.last_check_at, now)
        };
        if forced && !cfg.network_consent {
            tracing::warn!("请求了强制检查更新，但用户尚未同意联网，已跳过");
        }
        if gate {
            if forced {
                // 刻意不写 last_check_at：调试用的强制检查不该消耗掉真实的节流窗口，
                // 否则跑一次调试就会让接下来 24h 的自动检查失效，反而更难验证。
                tracing::warn!("强制检查更新（调试入口）：已绕过 24h 节流与自动检查开关");
            } else {
                // 发起前就记时间：失败也算"查过"，免得每次开设置都重试
                app_state.borrow_mut().mark_update_checked(now);
            }
            app_state.borrow().spawn_silent_update_check();
        }
    }

    let ui = if compat_as_main {
        // 同词库窗：不建设置外壳，静态色控件的亮暗双态要自己补上。
        crate::widgets::init_dark_sig(app_state.borrow().dark);
        compat_window::build_content(app_state.clone())
    } else if dict_as_main {
        // 词库窗自己不建设置外壳，但静态色控件（图标 tint / 渐变 / 淡底）的亮暗双态
        // 是全局注册的，平时由 `shell::build` 顺手做掉——走这条路就得自己补上，否则
        // 标题栏图标在暗色主题下是黑的。
        crate::widgets::init_dark_sig(app_state.borrow().dark);
        dict_window::build_content(app_state.clone())
    } else {
        shell::build(&app_state)
    };

    // 关窗守卫：有未保存改动时挡下这一次关闭，弹确认框问一句。
    //
    // `on_close_request` 的返回值被平台在 WM_CLOSE 里**同步等待**，容不下"等用户回答"
    // 这种异步决定，故这里只能"先返回 false 挡住 + 另起一条路把答案送回来"——那条路就是
    // 应用内的确认对话框，它的两个退出按钮走 `ctx.force_close()`（绕过本守卫）——
    // 判据本身（有没有改动）不会因为用户选了"直接退出"就消失，走普通关闭会被自己再拦一次。
    //
    // 自绘标题栏的 × 与 Alt+F4 都会到这里（windui 0.12 起两者同走关闭决策链）。
    if compat_as_main {
        let compat_for_activate = app_state.borrow().compat_mgr.clone();
        app = app.on_window_activated(move |_ctx, active| {
            // 同 `compat_window::open`：回到本窗就刷新。
            if active {
                compat_for_activate.request_refresh();
            }
        });
    }
    // 诊断信息对话框的「每 2 秒自动刷新」。定时器挂在主窗上、随主窗生灭（重建的主窗在
    // `open_main_window` 里另挂一份）。对话框关着或没勾时回调什么都不做——不写信号就不出帧
    // （见 `App::on_interval` 的文档），代价只是每 2 秒一次平台定时器唤醒。
    // 首窗不是设置主窗时不挂，保证任何时刻只有一份（见 `app_hosts_settings_shell`）。
    if app_hosts_settings_shell(dict_as_main, compat_as_main) {
        let tick_state = app_state.clone();
        app = app.on_interval(pages::memory::TICK, move |_ctx| {
            pages::memory::on_tick(&tick_state)
        });
    }
    let close_state = app_state.clone();
    app = app.on_close_request(move |_ctx| {
        let Ok(st) = close_state.try_borrow() else {
            // 借用不到说明正处在某个改状态的回调里，此时拦下反而更糟（用户会以为窗口卡死）。
            return true;
        };
        // 本窗承载的是词库时直接放行：词库的写操作即时生效，没有"未保存改动"这回事，
        // 拿设置页的判据拦它只会莫名其妙地弹确认框。复位标志，好让之后的
        // 「词库管理」重新开得出窗（此刻可能还有设置子窗开着，进程不一定退出）。
        if st.dict_is_main() {
            st.set_dict_is_main(false);
            return true;
        }
        // 应用兼容窗的改动即时落盘，同样没有「未保存改动」。
        if compat_as_main {
            return true;
        }
        // 清单只算一次，它自己就是守卫的判据（同子窗那份）。
        let unsaved = st.unsaved_items();
        if unsaved.is_empty() {
            // 放行的这一刻才记下主窗已关：二次实例据此分辨该 activate 已有主窗还是
            // 重建一个。必须在**放行分支**里 —— 下面那条是拦下，主窗还在。
            st.mark_main_window_closed();
            return true;
        }
        st.prime_exit_confirm(&unsaved);
        false
    });

    // 内存诊断：运行期每 2s 打一个净堆采样点，用于分辨「切页涨的内存」到底在 Rust 堆
    // 上还是在图形栈里。开着它会持续唤醒消息循环，与 d2d 的"空闲零 CPU"设计相冲，
    // 所以只在 memstat 构建里挂。
    #[cfg(feature = "memstat")]
    let app = app.on_interval(std::time::Duration::from_secs(2), |_| {
        memstat::mark("[运行期]")
    });

    memstat::mark("[run 之前]");
    app.bg(theme_def::theme_for(start_dark).palette.bg)
        // `--screenshot <path>` 离屏出图（配合 WIND_RPC_MOCK=1 可无 core 核对 UI）。
        .screenshot_from_args()
        .content(ui)
        .run();
}

/// UI 就绪后才能执行的导入动作（解析在 argv 侧，执行在首帧的 channel 回调里）。
enum ImportAction {
    /// `--page import` 深链：切高级页 + 弹「导入配置」+ 读一次剪贴板。
    ClipboardDialog,
    /// `.wpkg` 文件关联双击：走统一分派器进方案包导入确认。
    File(std::path::PathBuf),
}

/// 导入动作统一落点（冷启动与二次唤起同路）。
///
/// 文件分支不切页：方案包导入确认对话框挂在根上（全窗模态），与当前页无关；
/// 剪贴板分支切到高级页 —— 「导入配置」对话框关掉后用户应落在它的入口所在页。
fn run_import_action(
    ctx: &mut windui::core::EventCtx,
    act: ImportAction,
    state: &Rc<RefCell<state::AppState>>,
) {
    let st = state.borrow();
    match act {
        ImportAction::ClipboardDialog => {
            if let Some(idx) = cli::page_to_index("advanced") {
                st.tab.set(idx);
            }
            st.config_import.open();
            st.config_import.read_clipboard(ctx);
        }
        ImportAction::File(p) => {
            let ov = pages::config_import::ThemePkgConfirmSignals::of(&st);
            pages::config_import::dispatch_import_path(&st.config_import, &st.schema_mgr, ov, p);
        }
    }
}

/// 协议 URL 路由：解析 → 切到相关页；主题导入暂存到待确认状态并弹出导入对话框；
/// 方案包（schema）仅接受官方域，下载到临时文件后复用 schema_manager 既有的导入
/// 确认对话框；其余 kind 提示暂不支持。反馈走 feedback（toast）——底栏只剩状态点，
/// 没有常驻文字位。
fn route_protocol(raw: &str, state: &Rc<RefCell<state::AppState>>) {
    match protocol::parse_protocol_url(raw) {
        Ok(req) => {
            let st = state.borrow();
            // 切到**这类导入的落点页**，让用户看得见接下来要确认的东西：主题导入的对话框
            // 属于外观页、方案包导入的确认框属于方案页。不认识的 kind 不切页——只弹一句
            // "暂不支持"，把用户留在原处。
            //
            // 此前这里是"theme 去 3、其余一律去 4"的两档粗分，schema 落到 4 纯属遗留：
            // 4 是词库，与方案包导入毫无关系；而词库现在已迁出主窗、那一格是永不显示的
            // 占位，再送过去就是一屏空白——用户点了个导入链接，设置窗跳出来却什么都没有。
            let target_page = match req.kind.as_str() {
                "theme" => cli::page_to_index("ui"),
                "schema" => cli::page_to_index("schema"),
                _ => None,
            };
            if let Some(i) = target_page {
                st.tab.set(i);
            }
            tracing::info!(
                "协议导入请求: kind={} url={} name={}",
                req.kind,
                req.url,
                req.name
            );
            if req.kind == "theme" {
                st.set_pending_theme_import(req.url, req.name);
                st.theme_import_dialog_visible.set(true);
                st.feedback.set(Some((
                    ToastKind::Info,
                    "收到主题导入请求，请确认来源后导入".to_string(),
                )));
            } else if req.kind == "schema" {
                st.clear_pending_theme_import();
                st.theme_import_dialog_visible.set(false);
                // 白名单逐跳校验（域名不对不发请求，重定向出官方域即拒）。临时文件的
                // 生命周期跟着句柄走：确认对话框是异步的，句柄交给对话框，确认 / 取消后
                // 随待导入项释放即删；预览失败时在这里 drop 即删（见 schema_url_import）。
                match schema_url_import::download_schema_package(&req.url) {
                    Ok(pkg) => match rpc::call_core(
                        "scheme.previewImport",
                        &serde_json::json!({ "path": pkg.path().to_string_lossy() }),
                    ) {
                        Ok(v) => st.schema_mgr.open_downloaded_import_confirm(pkg, &v),
                        Err(e) => st
                            .feedback
                            .set(Some((ToastKind::Error, format!("读取方案包失败：{e}")))),
                    },
                    Err(e) => st.feedback.set(Some((ToastKind::Error, e))),
                }
            } else {
                st.clear_pending_theme_import();
                st.theme_import_dialog_visible.set(false);
                st.feedback.set(Some((
                    ToastKind::Info,
                    format!("暂不支持通过协议导入 {}", req.kind),
                )));
            }
        }
        Err(e) => {
            let st = state.borrow();
            st.clear_pending_theme_import();
            st.theme_import_dialog_visible.set(false);
            st.feedback
                .set(Some((ToastKind::Error, format!("协议解析失败：{e}"))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_protocol_stores_theme_import_request() {
        let state = Rc::new(RefCell::new(state::AppState::empty()));

        route_protocol(
            "windinput://import/theme?url=https%3A%2F%2Fexample.com%2Fdark.toml&name=Dark",
            &state,
        );

        let st = state.borrow();
        assert_eq!(st.tab.get(), crate::cli::page_to_index("ui").unwrap());
        assert_eq!(
            st.pending_theme_import_url.get(),
            "https://example.com/dark.toml"
        );
        assert_eq!(st.pending_theme_import_name.get(), "Dark");
        assert!(st.theme_import_dialog_visible.get());
        assert_eq!(
            st.feedback.get(),
            Some((
                ToastKind::Info,
                "收到主题导入请求，请确认来源后导入".to_string()
            ))
        );
    }

    #[test]
    fn route_protocol_reports_unsupported_non_theme_import() {
        let state = Rc::new(RefCell::new(state::AppState::empty()));

        route_protocol(
            "windinput://import/theme?url=https%3A%2F%2Fexample.com%2Fdark.toml&name=Dark",
            &state,
        );
        route_protocol(
            "windinput://import/dict?url=https%3A%2F%2Fexample.com%2Fdict.zip&name=Dict",
            &state,
        );

        let st = state.borrow();
        // 不支持的 kind **不切页**：停在上一次主题请求切到的外观页，而不是被送去词库
        // 那一格永不显示的占位（切过去就是一屏空白）。
        assert_eq!(st.tab.get(), crate::cli::page_to_index("ui").unwrap());
        assert_eq!(st.pending_theme_import_url.get(), "");
        assert_eq!(st.pending_theme_import_name.get(), "");
        assert!(!st.theme_import_dialog_visible.get());
        assert_eq!(
            st.feedback.get(),
            Some((ToastKind::Info, "暂不支持通过协议导入 dict".to_string()))
        );
    }

    /// schema kind 已接通，但只认官方域：非白名单域在**发出任何网络请求之前**
    /// 被拒并 toast（本测试因此不需要网络）。
    #[test]
    fn route_protocol_schema_rejects_non_official_domain() {
        let state = Rc::new(RefCell::new(state::AppState::empty()));

        route_protocol(
            "windinput://import/schema?url=https%3A%2F%2Fexample.com%2Fpkg.zip&name=Pkg",
            &state,
        );

        let st = state.borrow();
        // 方案包导入的确认框属于**方案页**，不是词库（旧代码 "theme=3、其余=4" 的两档
        // 粗分让它落在了 4，那是词库，与方案包毫无关系）。
        assert_eq!(st.tab.get(), crate::cli::page_to_index("schema").unwrap());
        assert!(!st.theme_import_dialog_visible.get());
        let (kind, msg) = st.feedback.get().expect("应有反馈");
        assert_eq!(kind, ToastKind::Error);
        assert!(msg.contains("官方域名"), "{msg}");
    }

    #[test]
    fn route_protocol_reports_parse_error() {
        let state = Rc::new(RefCell::new(state::AppState::empty()));

        route_protocol(
            "windinput://import/theme?url=https%3A%2F%2Fexample.com%2Fdark.toml&name=Dark",
            &state,
        );
        route_protocol("windinput://import/theme", &state);

        let st = state.borrow();
        let (kind, msg) = st.feedback.get().expect("应有反馈");
        assert_eq!(kind, ToastKind::Error);
        assert!(msg.starts_with("协议解析失败："));
        assert_eq!(st.pending_theme_import_url.get(), "");
        assert_eq!(st.pending_theme_import_name.get(), "");
        assert!(!st.theme_import_dialog_visible.get());
    }

    /// `--page dict` 必须**开窗**而不是切页：主窗那一格是永不显示的占位，切过去就是
    /// 一屏空白。这是本次把词库迁出主窗后最容易回退的一条。
    #[test]
    fn dict_target_requests_window_and_leaves_tab_alone() {
        let st = state::AppState::empty();
        let before = st.tab.get();

        route_target(
            &st,
            cli::page_to_index("dict").expect("dict 必在 PAGES 中"),
            false,
        );

        assert_eq!(st.tab.get(), before, "开词库窗不该动主窗的当前页");
        assert!(st.take_dict_window_request(), "应挂起一个开窗请求");
    }

    /// 开窗请求取一次即清：否则 `apply_loaded` 与 UI 通道两处都会消费到它，
    /// 一次深链开两回窗（非单例窗就是真开两个）。
    #[test]
    fn dict_window_request_is_consumed_once() {
        let st = state::AppState::empty();

        st.request_dict_window(false);

        assert!(st.take_dict_window_request());
        assert!(!st.take_dict_window_request(), "第二次取应为空");
    }

    /// 其余页照旧切页，且不该顺带挂起开窗请求。
    #[test]
    fn non_dict_target_switches_tab() {
        let st = state::AppState::empty();
        let about = cli::page_to_index("about").expect("about 必在 PAGES 中");

        route_target(&st, about, false);

        assert_eq!(st.tab.get(), about);
        assert!(!st.take_dict_window_request());
    }

    /// 「主窗已关闭」默认为假，置位后保持 —— 二次实例据此分辨该 activate 已有主窗
    /// 还是重建一个。默认为真的话，第一次点设置就会平白多开一个主窗。
    /// `App` 那个窗口的单例键必须跟着它承载的界面走。
    ///
    /// 一律登记成「main」的后果：`--page dict` 冷启动后点「设置」，`open_main_window`
    /// 的单例判定命中这个词库窗，平台把它激活到前台就算完事——设置窗永远开不出来，
    /// 而这一路没有任何报错。
    #[test]
    fn main_window_single_key_follows_what_it_hosts() {
        assert_eq!(main_window_single_key(false), MAIN_SINGLE_KEY);
        assert_eq!(main_window_single_key(true), dict_window::SINGLE_KEY);
        assert_ne!(
            MAIN_SINGLE_KEY,
            dict_window::SINGLE_KEY,
            "两个键同值的话，设置窗与词库窗会互相顶替"
        );
    }

    /// 主窗关了，诊断对话框的自动刷新就得停：冷启动先开的是词库 / 兼容窗时，App 级
    /// 定时器挂在那个窗口上、主窗关掉后仍在跑，不复位就会在后台一直取数。
    #[test]
    fn closing_main_window_stops_memory_auto_refresh() {
        let st = state::AppState::empty();
        st.memory.visible.set(true);
        st.memory.auto.set(true);

        st.mark_main_window_closed();

        assert!(!st.memory.visible.get());
        assert!(!st.memory.auto.get());
    }

    /// App 级的 2 秒定时器只在 `App` 那个窗口就是设置主窗时挂：词库 / 兼容窗当首窗时，
    /// 之后开出的主窗由 `open_main_window` 自挂一份，两份同时跑就是每 2 秒取两次。
    #[test]
    fn app_level_memory_tick_only_when_first_window_is_settings() {
        assert!(app_hosts_settings_shell(false, false));
        assert!(!app_hosts_settings_shell(true, false));
        assert!(!app_hosts_settings_shell(false, true));
    }

    #[test]
    fn main_window_closed_starts_false_and_latches() {
        let st = state::AppState::empty();
        assert!(!st.main_window_closed.get());

        st.mark_main_window_closed();

        assert!(st.main_window_closed.get());
    }

    /// 词库作为 `App` 主窗时不得再开第二个词库窗：`App::run` 建的窗口拿不到
    /// `Window::single` 的键，单例登记表里没有它，挡不住重复开窗。
    #[test]
    fn dict_as_main_window_suppresses_extra_dict_window() {
        let st = state::AppState::empty();
        st.set_dict_is_main(true);

        st.request_dict_window(false);

        assert!(
            !st.take_dict_window_request(),
            "词库已是主窗，不该再挂起开窗请求"
        );
    }

    /// 复位后恢复正常：词库主窗关掉、只剩设置子窗时，「词库管理」还得开得出来。
    #[test]
    fn dict_window_request_works_again_after_main_dict_closed() {
        let st = state::AppState::empty();
        st.set_dict_is_main(true);
        st.set_dict_is_main(false);

        st.request_dict_window(false);

        assert!(st.take_dict_window_request());
    }

    /// 重开主窗的请求同样取一次即清，否则一次「点设置」会重建两个主窗。
    #[test]
    fn main_window_request_is_consumed_once() {
        let st = state::AppState::empty();

        st.request_main_window();

        assert!(st.take_main_window_request());
        assert!(!st.take_main_window_request(), "第二次取应为空");
    }
}
