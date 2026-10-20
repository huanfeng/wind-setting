//! 用户分库（设计 `user-dict-libraries-and-external-edit.md` §3.3）：中栏「用户词库」下的
//! 分库节点（选中、开关、新建 / 改名 / 删除）与「从文件导入为分库」。中栏本身见 `category_nav`。
//!
//! 选中分库时当前类别变成 `WordCategory::UserLib`：列表走 `userDicts.entries`、删除走
//! `userDicts.removeEntry`，工具栏与右键菜单照常由 spec 的 ops 决定（只剩搜索、删除与复制）。
//! 选中哪个分库按方案记在会话内存里；分库列表只在切到该方案、或显式刷新时取一次
//! （core 取列表要逐个解析文件，不随翻页 / 搜索重取）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{Value, json};
use windui::core::EventCtx;
use windui::prelude::*;

use super::spec::LogicalTab;
use super::state::DictManagerState;
use crate::rpc;

/// 一个用户分库（`userDicts.list` 的一项）。
#[derive(Clone, Debug, PartialEq)]
pub struct UserLib {
    /// 文件主名，RPC 里的 `name`。
    pub name: String,
    /// 显示名（词库头的 `name:`，没有则同 `name`）。
    pub label: String,
    pub enabled: bool,
    pub entries: u64,
    /// 相对数据目录的路径。
    pub file: String,
    /// 解析问题（同 `data.endEdit` 的 issues 形状）。
    pub issues: Vec<Value>,
}

/// `userDicts.list` 的返回 → 分库列表（缺 `name` 的项丢弃）。
pub(crate) fn parse_libs(v: &Value) -> Vec<UserLib> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|it| {
            let s = |k: &str| it.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let name = it.get("name")?.as_str()?.to_string();
            let label = Some(s("label"))
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| name.clone());
            Some(UserLib {
                label,
                enabled: it.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                entries: it.get("entries").and_then(Value::as_u64).unwrap_or(0),
                file: s("file"),
                issues: it
                    .get("issues")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                name,
            })
        })
        .collect()
}

pub(crate) const MAIN_LABEL: &str = "主词库";
/// 主词库的条目数：取一条，读总数。
fn main_entries(schema: &str) -> Option<u64> {
    rpc::call_core(
        "dict.listPaged",
        &json!({ "schemaId": schema, "offset": 0, "limit": 1 }),
    )
    .ok()?
    .get("total")?
    .as_u64()
}

/// 分库名的即时校验（与 core `validate_name` 同规则；core 仍是最终判据），外加重名检查。
/// `renaming` 是改名时的原名。
pub(crate) fn validate_name(
    name: &str,
    libs: &[UserLib],
    renaming: Option<&str>,
) -> Result<(), String> {
    const FORBIDDEN: &str = "/\\:*?\"<>|";
    if name.is_empty() {
        return Err("请输入名字".into());
    }
    if name.starts_with(char::is_whitespace) || name.ends_with(char::is_whitespace) {
        return Err("首尾不能有空格".into());
    }
    if name.chars().count() > 64 {
        return Err("最多 64 个字".into());
    }
    if name.chars().any(|c| FORBIDDEN.contains(c)) {
        return Err("不能含 / \\ : * ? \" < > |".into());
    }
    if name.chars().any(char::is_control) {
        return Err("不能含控制字符".into());
    }
    if name.starts_with('.') || name.ends_with('.') {
        return Err("不能以 . 开头或结尾".into());
    }
    if renaming == Some(name) {
        return Err("名字没有变".into());
    }
    // 只改大小写的重命名放行：撞上的就是它自己。
    let taken = libs
        .iter()
        .any(|l| l.name.to_lowercase() == name.to_lowercase() && Some(l.name.as_str()) != renaming);
    if taken {
        return Err("已有同名分库".into());
    }
    Ok(())
}

/// 导入时默认的分库名：文件主名，去掉 `.dict` 尾巴（`成语.dict.yaml` → `成语`）。
pub(crate) fn name_from_file(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match stem.strip_suffix(".dict") {
        Some(s) => s.to_string(),
        None => stem,
    }
}

/// 选中分库有解析问题时表格上方的提示；没问题为空串。
pub(crate) fn notice_text(lib: &UserLib) -> String {
    match super::file_edit::issues_text(&lib.issues) {
        Some(p) => format!("⚠ 分库「{}」：{p}", lib.label),
        None => String::new(),
    }
}

/// 「从文件导入」完成的提示。
pub(crate) fn import_text(name: &str, v: &Value, unencodable: usize) -> String {
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let mut msg = format!("已导入 {} 条到分库「{name}」", n("imported"));
    if n("skipped") > 0 {
        msg.push_str(&format!("；跳过 {} 行", n("skipped")));
    }
    if unencodable > 0 {
        msg.push_str(&format!("；{unencodable} 个词出不了码已跳过"));
    }
    msg
}

/// 「打开目录」要打开的目录：方案数据目录下的 `dicts`，还没有就打开方案数据目录本身。
pub(crate) fn dir_to_open(base: &Path) -> PathBuf {
    let dicts = base.join("dicts");
    if dicts.is_dir() {
        dicts
    } else {
        base.to_path_buf()
    }
}

/// 分库的「编辑文件」参数。
pub(crate) fn lib_edit_target(schema: &str, name: &str) -> Value {
    json!({ "kind": "user_dict", "schemaId": schema, "name": name })
}

/// 「移动到」的目标：主词库或某个分库（文件主名）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoveTarget {
    Main,
    Lib(String),
}

/// 名字对话框的用途。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Prompt {
    Create,
    Rename {
        old: String,
    },
    /// 从文件导入：路径给 core 读（不受帧长限制），内容留在本进程判断是不是纯词列表。
    Import {
        path: String,
        content: String,
    },
}

/// 分库节点与名字对话框的状态（Signal 为 Copy，整体可 Clone 进闭包）。
#[derive(Clone)]
pub struct UserLibsState {
    /// 当前方案的分库。
    pub list: Signal<Vec<UserLib>>,
    /// 取列表成功过（这一版 core 认 `userDicts.*`）；否则中栏的「用户词库」不分主库与分库。
    pub available: Signal<bool>,
    /// 当前方案记着的分库（`None` = 主词库），中栏节点的选中态读它。
    pub current: Signal<Option<String>>,
    /// 主词库的条目数（中栏显示；取不到为 `None`）。
    pub main_count: Signal<Option<u64>>,
    /// 选中分库的解析问题提示；空 = 不显示。
    pub notice: Signal<String>,
    /// 各方案选中的分库（方案 id → 分库名；没有 = 主词库）。会话内记忆。
    pub(super) memory: Rc<RefCell<HashMap<String, String>>>,
    /// 列表是为哪个方案取的；与当前不同时 [`DictManagerState::reload`] 重取一次。
    loaded_for: Rc<RefCell<Option<String>>>,

    prompt: Rc<RefCell<Option<Prompt>>>,
    pub prompt_visible: Signal<bool>,
    pub prompt_title: Signal<String>,
    pub name: Signal<String>,
    /// 名字的即时校验结果；空 = 没问题。
    pub name_error: Signal<String>,
    /// 名字对话框是不是「从文件导入」（驱动文件 / 格式 / 权重几行的显隐）。
    pub importing: Signal<bool>,
    pub file_label: Signal<String>,
    /// 导入格式：0 = 自动识别，1 = 纯词列表（同导入对话框）。
    pub format: Signal<usize>,
    /// 格式行右侧的识别结果（纯词列表时报词数；其余交给 core 识别，留空）。
    pub format_note: Signal<String>,
}

impl UserLibsState {
    pub fn new() -> Self {
        Self {
            list: signal(Vec::new()),
            available: signal(false),
            current: signal(None),
            main_count: signal(None),
            notice: signal(String::new()),
            memory: Rc::new(RefCell::new(HashMap::new())),
            loaded_for: Rc::new(RefCell::new(None)),
            prompt: Rc::new(RefCell::new(None)),
            prompt_visible: signal(false),
            prompt_title: signal(String::new()),
            name: signal(String::new()),
            name_error: signal(String::new()),
            importing: signal(false),
            file_label: signal(String::new()),
            format: signal(0),
            format_note: signal(String::new()),
        }
    }
}

impl Default for UserLibsState {
    fn default() -> Self {
        Self::new()
    }
}

impl DictManagerState {
    /// 当前选中的分库名：只在方案域的「用户词库」子标签上有；`None` = 主词库。
    pub(crate) fn selected_lib(&self) -> Option<String> {
        if self.current_tab() != LogicalTab::UserDict {
            return None;
        }
        let id = self.current_schema_id()?;
        self.libs.memory.borrow().get(&id).cloned()
    }

    /// 中栏的「用户词库」要不要分出主词库与各分库：方案有用户词库、core 认分库。
    pub fn has_lib_group(&self) -> bool {
        self.lib_scope().is_some() && self.libs.available.get()
    }

    /// 列表还不是为当前方案取的就取一次，再让选中态跟上（[`Self::reload`] 开头调用）。
    pub(crate) fn ensure_user_libs(&self) {
        if *self.libs.loaded_for.borrow() != self.lib_scope() {
            self.refresh_user_libs();
        } else {
            self.sync_lib_selection();
        }
    }

    /// 分库列表的作用域：有「用户词库」的方案域的方案 id。中栏不论停在哪一类都列出分库。
    fn lib_scope(&self) -> Option<String> {
        if self.tabs_for_domain().contains(&LogicalTab::UserDict) {
            self.current_schema_id()
        } else {
            None
        }
    }

    /// 重取分库列表并同步中栏；记着的分库已不存在就回到主词库。
    pub fn refresh_user_libs(&self) {
        let scope = self.lib_scope();
        *self.libs.loaded_for.borrow_mut() = scope.clone();
        let (libs, ok) = match scope.as_ref().map(|_| self.lib_switch_schema()) {
            Some(Some(id)) => match rpc::call_core("userDicts.list", &json!({ "schemaId": id })) {
                Ok(v) => (parse_libs(&v), true),
                Err(e) => {
                    tracing::debug!("取用户分库列表失败：{e}");
                    (Vec::new(), false)
                }
            },
            _ => (Vec::new(), false),
        };
        let mut fell_back = false;
        if let Some(id) = &scope {
            let mut mem = self.libs.memory.borrow_mut();
            if mem
                .get(id)
                .is_some_and(|n| !libs.iter().any(|l| &l.name == n))
            {
                mem.remove(id);
                fell_back = true;
            }
        }
        // 正在看的分库没了（删掉、外面挪走）：回到主词库，像切类别那样清掉分页、排序与勾选——
        // 勾着的是分库的行，留着会让「删除选中」按 (编码, 词) 去删主词库里的同名词。
        if fell_back && self.current_tab() == LogicalTab::UserDict {
            self.reset_table_view();
        }
        // 主词库条目数：停在主词库上时表格加载自会给出总数（`sync_lib_count`），不必为它
        // 再扫一遍整表——`dict.listPaged` 取一条也要扫全表才算得出总数。
        let main_count = match (&scope, ok) {
            (Some(_), true) if self.current_category() == super::spec::WordCategory::UserDict => {
                self.libs.main_count.get()
            }
            (Some(id), true) => main_entries(id),
            _ => None,
        };
        let changed = ok != self.libs.available.get()
            || libs != self.libs.list.get()
            || main_count != self.libs.main_count.get();
        self.libs.available.set(ok);
        self.libs.list.set(libs);
        self.libs.main_count.set(main_count);
        if changed {
            self.bump_nav_epoch();
        }
        self.sync_lib_selection();
    }

    /// 读开关时以哪个方案为准：拼音族折叠成一个域时取成员里的活动方案（活动方案不在域内就
    /// 取代表方案）；不折叠的域就是它自己。条目与文件是成员共用的，只有开关按方案各存一份。
    fn lib_switch_schema(&self) -> Option<String> {
        let rep = self.current_schema_id()?;
        let members = self.current_domain_members();
        if members.len() <= 1 {
            return Some(rep);
        }
        let active = rpc::call_core("schema.active", &json!({}))
            .ok()
            .and_then(|v| v.get("id").and_then(Value::as_str).map(String::from));
        Some(active.filter(|a| members.contains(a)).unwrap_or(rep))
    }

    /// 中栏选中态与问题提示跟着当前选中走。
    pub(crate) fn sync_lib_selection(&self) {
        let libs = self.libs.list.get();
        let remembered = self
            .current_schema_id()
            .and_then(|id| self.libs.memory.borrow().get(&id).cloned());
        if self.libs.current.get() != remembered {
            self.libs.current.set(remembered);
        }
        let sel = self.selected_lib();
        let notice = sel
            .and_then(|n| libs.iter().find(|l| l.name == n).map(notice_text))
            .unwrap_or_default();
        if self.libs.notice.get() != notice {
            self.libs.notice.set(notice);
        }
    }

    /// 下次重载时重取分库列表与主词库条目数（转正等会改到主词库的写操作之前调）。
    pub(crate) fn invalidate_user_libs(&self) {
        *self.libs.loaded_for.borrow_mut() = None;
    }

    /// 重取分库列表再重载表格（「刷新」、外部编辑完成之后用）。
    pub fn reload_with_libs(&self) {
        *self.libs.loaded_for.borrow_mut() = None;
        self.reload();
    }

    /// 选中主词库（`None`）或某个分库：记下并像切类别那样重载（归零分页、清排序与勾选、
    /// 整表重建）。只改「用户词库」内的选择，切到这一类由调用方（中栏）负责。
    pub fn select_lib(&self, want: Option<String>) {
        let Some(id) = self.current_schema_id() else {
            return;
        };
        let remembered = self.libs.memory.borrow().get(&id).cloned();
        if want == remembered {
            return;
        }
        match want {
            Some(n) => self.libs.memory.borrow_mut().insert(id, n),
            None => self.libs.memory.borrow_mut().remove(&id),
        };
        self.sync_lib_selection();
        if self.current_tab() != LogicalTab::UserDict {
            return;
        }
        self.reset_table_view();
        self.reload();
    }

    /// 换了一张表：归零分页、清状态文字、排序与勾选，整表重建（列集可能不同）。
    fn reset_table_view(&self) {
        *self.offset.borrow_mut() = 0;
        self.status.set(String::new());
        self.sort.set(None);
        self.clear_selection();
        self.bump_table_epoch();
    }

    /// 主词库 / 分库的表格重载后，中栏的条目数跟上表格的总数（增删了词不必等刷新）。
    /// 搜索时总数是命中数，不拿来改。
    pub(crate) fn sync_lib_count(&self) {
        use super::spec::WordCategory;
        if !self.search.get().trim().is_empty() || !self.has_lib_group() {
            return;
        }
        let total = self.total.get() as u64;
        if self.current_category() == WordCategory::UserDict {
            if self.libs.main_count.get() != Some(total) {
                self.libs.main_count.set(Some(total));
                self.bump_nav_epoch();
            }
            return;
        }
        if self.current_category() != WordCategory::UserLib {
            return;
        }
        let Some(name) = self.selected_lib() else {
            return;
        };
        let mut libs = self.libs.list.get();
        if let Some(l) = libs
            .iter_mut()
            .find(|l| l.name == name && l.entries != total)
        {
            l.entries = total;
            self.libs.list.set(libs);
            self.bump_nav_epoch();
        }
    }

    // ── 移动到别的词库 ─────────────────────────────────────

    /// 当前表格的词条能移去哪儿：主词库与各分库，去掉当前这一个。不在用户词库、或没有
    /// 分库能力时为空（不显示入口）。
    pub fn move_targets(&self) -> Vec<(String, MoveTarget)> {
        use super::spec::WordCategory;
        let cat = self.current_category();
        if !self.has_lib_group() || !matches!(cat, WordCategory::UserDict | WordCategory::UserLib) {
            return Vec::new();
        }
        let cur = self.selected_lib();
        let mut out = Vec::new();
        if cur.is_some() {
            out.push((MAIN_LABEL.to_string(), MoveTarget::Main));
        }
        out.extend(
            self.libs
                .list
                .get()
                .into_iter()
                .filter(|l| Some(&l.name) != cur.as_ref())
                .map(|l| {
                    let label = if l.enabled {
                        l.label
                    } else {
                        format!("{}（已停用）", l.label)
                    };
                    (label, MoveTarget::Lib(l.name))
                }),
        );
        out
    }

    /// 移动一条 = 先加到目标、再从当前删（加失败则原条目不动）。没写权重的（0）不带权重，
    /// 由目标按方案默认权重处理。
    pub(crate) fn move_calls(
        &self,
        rows: &[super::spec::WordRow],
        to: &MoveTarget,
    ) -> Vec<[(&'static str, Value); 2]> {
        let Some(id) = self.current_schema_id() else {
            return Vec::new();
        };
        rows.iter()
            .map(|r| {
                let mut add = json!({ "schemaId": id, "code": r.code, "text": r.text });
                if r.weight != 0 {
                    add["weight"] = json!(r.weight);
                }
                let method = match to {
                    MoveTarget::Main => "dict.add",
                    MoveTarget::Lib(name) => {
                        add["name"] = json!(name);
                        "userDicts.addEntry"
                    }
                };
                [(method, add), self.delete_call(&r.code, &r.text)]
            })
            .collect()
    }

    /// 把这些词条移到 `to`（`label` 是提示里的名字）。逐条执行，首错即停并报已移了几条。
    pub fn move_rows(&self, rows: Vec<super::spec::WordRow>, to: &MoveTarget, label: &str) {
        let calls = self.move_calls(&rows, to);
        let n = calls.len();
        let mut done = 0;
        'rows: for (i, pair) in calls.into_iter().enumerate() {
            for (step, (m, p)) in pair.into_iter().enumerate() {
                if let Err(e) = rpc::call_core(m, &p) {
                    // 第二步（从原处删）失败时，这一条已经加进目标：两边各有一份，要说清楚。
                    let extra = if step == 1 {
                        format!("；「{}」已加到目标，但没从原处删掉", rows[i].text)
                    } else {
                        String::new()
                    };
                    self.notify_err(&format!("已移动 {done}/{n} 条后失败：{e}{extra}"));
                    break 'rows;
                }
            }
            done += 1;
        }
        for r in &rows[..done] {
            self.deselect(&r.code, &r.text);
        }
        if done == n {
            let off = matches!(to, MoveTarget::Lib(name)
                if self.libs.list.get().iter().any(|l| &l.name == name && !l.enabled));
            // 菜单名里停用的分库已带「（已停用）」，挪到引号外。
            let label = label.strip_suffix("（已停用）").unwrap_or(label);
            let note = if off { "（已停用）" } else { "" };
            self.notify_ok(&format!("已移动 {n} 条到「{label}」{note}"));
        }
        // 两边的条目数都变了。
        self.reload_with_libs();
    }

    /// 「移动到…」的菜单项：每个目标一项，点了就移 `rows`。
    pub fn move_menu_items(&self, rows: Vec<super::spec::WordRow>) -> Vec<MenuItem> {
        self.move_targets()
            .into_iter()
            .map(|(label, to)| {
                let (m, rows, l) = (self.clone(), rows.clone(), label.clone());
                MenuItem::run(label, move |_| m.move_rows(rows.clone(), &to, &l), false)
            })
            .collect()
    }

    /// 勾选的那些词条的「移动到…」菜单。
    pub fn move_selected_menu_items(&self) -> Vec<MenuItem> {
        self.move_menu_items(self.selected_rows())
    }

    // ── 分库操作 ───────────────────────────────────────────

    /// 分库操作做完：重取列表；删掉 / 改名的正好是选中的那个时表格也要换。
    fn after_lib_change(&self) {
        self.reload_with_libs();
    }

    /// 开关一个分库，对域内每个成员方案都改（拼音族折叠时全拼、双拼各存一份开关）。任一成员
    /// 失败就把已改的成员改回去并提示；返回是否成功（列表行的勾选框据此回滚）。
    pub fn set_lib_enabled(&self, name: &str, enabled: bool) -> bool {
        let call = |id: &str, on: bool| {
            rpc::call_core(
                "userDicts.setEnabled",
                &json!({ "schemaId": id, "name": name, "enabled": on }),
            )
        };
        let mut done: Vec<String> = Vec::new();
        let mut failed = None;
        for id in self.current_domain_members() {
            match call(&id, enabled) {
                Ok(_) => done.push(id),
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = &failed {
            for id in done.iter().rev() {
                if let Err(e2) = call(id, !enabled) {
                    tracing::warn!("分库开关回滚失败（{id}）：{e2}");
                }
            }
            self.notify_err(&format!("切换失败：{e}"));
        }
        let ok = failed.is_none() && !done.is_empty();
        if ok {
            let label = self
                .libs
                .list
                .get()
                .into_iter()
                .find(|l| l.name == name)
                .map_or_else(|| name.to_string(), |l| l.label);
            let verb = if enabled { "已启用" } else { "已停用" };
            self.notify_ok(&format!("{verb}「{label}」"));
        }
        self.refresh_user_libs();
        ok
    }

    /// 分库的「编辑文件」。
    pub fn edit_lib_file(&self, name: &str) {
        let Some(id) = self.current_schema_id() else {
            return;
        };
        self.begin_and_open(&lib_edit_target(&id, name));
    }

    /// 「打开目录」：方案数据目录下的 `dicts`（还没有就打开方案数据目录）。
    pub fn open_libs_dir(&self) {
        let Some(id) = self.current_schema_id() else {
            return;
        };
        let base = match rpc::call_core("data.locate", &json!({ "schemaId": id })) {
            Ok(v) => v
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            Err(e) => {
                self.notify_err(&format!("找不到数据目录：{e}"));
                return;
            }
        };
        let dir = dir_to_open(Path::new(&base));
        if let Err(e) = super::file_edit::open_path(&dir.to_string_lossy()) {
            self.notify_err(&format!("没能打开文件夹（{e}），目录在 {}", dir.display()));
        }
    }

    /// 删除确认通过后：删文件、重取列表。
    pub(crate) fn do_delete_user_lib(&self, name: &str) {
        let Some(id) = self.current_schema_id() else {
            return;
        };
        match rpc::call_core("userDicts.delete", &json!({ "schemaId": id, "name": name })) {
            Ok(_) => self.notify_ok(&format!("已删除分库「{name}」")),
            Err(e) => self.notify_err(&format!("删除失败：{e}")),
        }
        self.after_lib_change();
    }

    // ── 名字对话框（新建 / 重命名 / 导入）───────────────────────

    pub fn open_lib_create(&self) {
        self.open_prompt(Prompt::Create, "新建分库", String::new());
    }

    pub fn open_lib_rename(&self, name: &str) {
        self.open_prompt(
            Prompt::Rename {
                old: name.to_string(),
            },
            "重命名分库",
            name.to_string(),
        );
    }

    /// 「从文件导入…」：选文件 → 名字对话框（默认取文件主名）。
    pub fn pick_lib_import_file(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        ctx.request_pick_file(
            PickDialog::new()
                .title("选择词库文件")
                .filter("词库文件 (Rime/TSV/WindDict/纯词列表)", &["yaml", "txt"]),
            move |path| {
                if let Some(path) = path.as_deref() {
                    state.open_lib_import(path);
                }
            },
        );
    }

    /// 选好文件后打开名字对话框。
    pub(crate) fn open_lib_import(&self, path: &Path) {
        let content = match crate::text_file::read_utf8(path) {
            Ok(c) => c,
            Err(e) => {
                self.notify_err(&e);
                return;
            }
        };
        self.libs.file_label.set(
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        self.libs.format.set(0);
        self.open_prompt(
            Prompt::Import {
                path: path.to_string_lossy().into_owned(),
                content,
            },
            "从文件导入为分库",
            name_from_file(path),
        );
        self.on_lib_format_changed();
    }

    fn open_prompt(&self, p: Prompt, title: &str, name: String) {
        self.libs.importing.set(matches!(p, Prompt::Import { .. }));
        *self.libs.prompt.borrow_mut() = Some(p);
        self.libs.prompt_title.set(title.to_string());
        self.libs.name.set(name);
        self.on_lib_name_changed();
        self.libs.prompt_visible.set(true);
    }

    pub fn cancel_lib_prompt(&self) {
        *self.libs.prompt.borrow_mut() = None;
        self.libs.prompt_visible.set(false);
    }

    /// 名字输入变化（effect 调用）：即时校验。空着不报错，只是「确定」不可点。
    pub fn on_lib_name_changed(&self) {
        let name = self.libs.name.get();
        let name = name.trim();
        let renaming = match &*self.libs.prompt.borrow() {
            Some(Prompt::Rename { old }) => Some(old.clone()),
            _ => None,
        };
        let err = if name.is_empty() {
            String::new()
        } else {
            validate_name(name, &self.libs.list.get(), renaming.as_deref())
                .err()
                .unwrap_or_default()
        };
        self.libs.name_error.set(err);
    }

    /// 名字对话框的「确定」可不可点。
    pub fn lib_prompt_ready(&self) -> bool {
        !self.libs.name.get().trim().is_empty() && self.libs.name_error.get().is_empty()
    }

    /// 这次导入按纯词列表处理：用户指定，或内容像纯词列表（同导入对话框的判据）。
    pub fn lib_import_is_word_list(&self) -> bool {
        match &*self.libs.prompt.borrow() {
            Some(Prompt::Import { content, .. }) => {
                self.libs.format.get() == 1 || super::wordlist::looks_like_word_list(content)
            }
            _ => false,
        }
    }

    /// 格式下拉变化（effect 调用）：纯词列表时回显词数。
    pub fn on_lib_format_changed(&self) {
        let note = match &*self.libs.prompt.borrow() {
            Some(Prompt::Import { content, .. }) if self.lib_import_is_word_list() => {
                let n = super::wordlist::parse_word_list(content).words.len();
                format!("纯词列表 · {n} 个词，按本方案出码")
            }
            _ => String::new(),
        };
        self.libs.format_note.set(note);
    }

    /// 「确定」：按用途新建 / 改名 / 导入。成功则关对话框并刷新，失败留着对话框可改了再试。
    pub fn submit_lib_prompt(&self) {
        let name = self.libs.name.get().trim().to_string();
        let prompt = self.libs.prompt.borrow().clone();
        let Some(prompt) = prompt else { return };
        let renaming = match &prompt {
            Prompt::Rename { old } => Some(old.as_str()),
            _ => None,
        };
        if let Err(e) = validate_name(&name, &self.libs.list.get(), renaming) {
            self.libs.name_error.set(e);
            return;
        }
        let Some(id) = self.current_schema_id() else {
            return;
        };
        let select_new = !matches!(prompt, Prompt::Rename { .. });
        let done = match prompt {
            Prompt::Create => self.lib_call(
                "userDicts.create",
                json!({ "schemaId": id, "name": name }),
                &format!("已新建分库「{name}」"),
            ),
            Prompt::Rename { old } => {
                let ok = self.lib_call(
                    "userDicts.rename",
                    json!({ "schemaId": id, "name": old, "newName": name }),
                    &format!("已改名为「{name}」"),
                );
                if ok {
                    let mut mem = self.libs.memory.borrow_mut();
                    if mem.get(&id) == Some(&old) {
                        mem.insert(id.clone(), name.clone());
                    }
                }
                ok
            }
            Prompt::Import { path, content } => self.import_lib(&id, &name, &path, &content),
        };
        if done {
            self.cancel_lib_prompt();
            self.after_lib_change();
            // 新建 / 导入的分库直接选中，不必再去中栏点一下。
            let tab = self
                .tabs_for_domain()
                .iter()
                .position(|t| *t == LogicalTab::UserDict);
            if let (true, Some(tab)) = (select_new, tab) {
                self.select_nav(&super::category_nav::NavNode::Lib(tab, name));
            }
        }
    }

    /// 调一个分库写操作，成功提示 `ok_msg`；失败提示 core 的原话。
    fn lib_call(&self, method: &str, params: Value, ok_msg: &str) -> bool {
        match rpc::call_core(method, &params) {
            Ok(_) => {
                self.notify_ok(ok_msg);
                true
            }
            Err(e) => {
                self.notify_err(&format!("操作失败：{e}"));
                false
            }
        }
    }

    /// 导入为分库：纯词列表先分批出码拼成 TSV 以 `content` 传，其余传路径由 core 识别。
    fn import_lib(&self, schema: &str, name: &str, path: &str, content: &str) -> bool {
        let mut params = json!({ "schemaId": schema, "name": name });
        let mut unencodable = 0;
        if self.lib_import_is_word_list() {
            let words = super::wordlist::parse_word_list(content).words;
            if words.is_empty() {
                self.notify_err("文件里没有可用的词条。");
                return false;
            }
            let weight = match crate::weight::parse_weight(&self.io_weight.get()) {
                Ok(w) => i64::from(w),
                Err(e) => {
                    self.notify_err(&e);
                    return false;
                }
            };
            let (rows, n) = match self.encode_word_batches(schema, &words) {
                Ok(r) => r,
                Err(e) => {
                    self.notify_err(&e);
                    return false;
                }
            };
            if rows.is_empty() {
                self.notify_err(&format!("{} 个词在本方案下都出不了码", words.len()));
                return false;
            }
            unencodable = n;
            params["content"] = json!(super::wordlist::to_tsv(&rows, weight));
        } else {
            params["path"] = json!(path);
        }
        match rpc::call_core("userDicts.importAs", &params) {
            Ok(v) => {
                self.notify_ok(&import_text(name, &v, unencodable));
                true
            }
            Err(e) => {
                self.notify_err(&format!("导入失败：{e}"));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib(name: &str, enabled: bool) -> UserLib {
        UserLib {
            name: name.into(),
            label: name.into(),
            enabled,
            entries: 0,
            file: format!("wb/dicts/{name}.dict.yaml"),
            issues: Vec::new(),
        }
    }

    #[test]
    fn parse_libs_reads_fields_and_issues() {
        let v = json!([
            { "name": "chengyu", "label": "成语", "enabled": true, "entries": 3,
              "file": "wb/dicts/chengyu.dict.yaml" },
            { "name": "行业词", "label": "行业词", "enabled": false, "entries": 0,
              "file": "wb/dicts/行业词.dict.yaml", "issues": [{ "kind": "isolated" }] },
            { "label": "没名字" }
        ]);
        let libs = parse_libs(&v);
        assert_eq!(libs.len(), 2, "缺 name 的项丢弃");
        assert_eq!(libs[0].label, "成语");
        assert_eq!(libs[0].entries, 3);
        assert!(libs[0].enabled && libs[0].issues.is_empty());
        assert!(!libs[1].enabled);
        assert_eq!(libs[1].issues, vec![json!({ "kind": "isolated" })]);
        assert!(parse_libs(&json!(null)).is_empty());
    }

    #[test]
    fn label_falls_back_to_name() {
        let libs = parse_libs(&json!([{ "name": "a", "label": "" }]));
        assert_eq!(libs[0].label, "a");
    }

    #[test]
    fn validate_name_follows_core_rules() {
        let libs = [lib("成语", true)];
        let bad = |n: &str| validate_name(n, &libs, None).unwrap_err();
        assert_eq!(bad(""), "请输入名字");
        assert_eq!(bad(" 词"), "首尾不能有空格");
        assert_eq!(bad(&"字".repeat(65)), "最多 64 个字");
        assert_eq!(bad("a/b"), "不能含 / \\ : * ? \" < > |");
        assert_eq!(bad("a|b"), "不能含 / \\ : * ? \" < > |");
        assert_eq!(bad("a\u{7}b"), "不能含控制字符");
        assert_eq!(bad(".词"), "不能以 . 开头或结尾");
        assert_eq!(bad("词."), "不能以 . 开头或结尾");
        assert_eq!(bad("成语"), "已有同名分库");
        assert!(validate_name(&"字".repeat(64), &libs, None).is_ok());
        assert!(validate_name("行业 词", &libs, None).is_ok());
    }

    #[test]
    fn validate_name_when_renaming() {
        let libs = [lib("成语", true), lib("Tech", true)];
        assert_eq!(
            validate_name("成语", &libs, Some("成语")).unwrap_err(),
            "名字没有变"
        );
        assert_eq!(
            validate_name("tech", &libs, Some("成语")).unwrap_err(),
            "已有同名分库",
            "Windows 文件名不分大小写"
        );
        assert!(
            validate_name("TECH", &libs, Some("Tech")).is_ok(),
            "只改大小写可以"
        );
    }

    #[test]
    fn name_from_file_strips_dict_suffix() {
        assert_eq!(name_from_file(Path::new("/x/成语.dict.yaml")), "成语");
        assert_eq!(name_from_file(Path::new("/x/行业词.txt")), "行业词");
        assert_eq!(name_from_file(Path::new("/x/词表")), "词表");
    }

    #[test]
    fn notice_text_names_the_library() {
        let mut l = lib("chengyu", true);
        l.label = "成语".into();
        assert_eq!(notice_text(&l), "");
        l.issues = vec![json!({ "kind": "bad_lines", "lines": 3 })];
        assert_eq!(notice_text(&l), "⚠ 分库「成语」：3 行格式不对，已跳过");
        l.issues = vec![json!({ "kind": "isolated" })];
        assert_eq!(notice_text(&l), "⚠ 分库「成语」：文件格式损坏，未载入");
    }

    #[test]
    fn import_text_reports_skips() {
        assert_eq!(
            import_text("成语", &json!({ "imported": 12, "skipped": 0 }), 0),
            "已导入 12 条到分库「成语」"
        );
        assert_eq!(
            import_text("成语", &json!({ "imported": 12, "skipped": 3 }), 2),
            "已导入 12 条到分库「成语」；跳过 3 行；2 个词出不了码已跳过"
        );
    }

    #[test]
    fn dir_to_open_prefers_dicts_subdir() {
        let base = std::env::temp_dir().join(format!("wind_ulibs_dir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        assert_eq!(dir_to_open(&base), base, "还没有 dicts 就开方案目录");
        std::fs::create_dir_all(base.join("dicts")).unwrap();
        assert_eq!(dir_to_open(&base), base.join("dicts"));
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── 状态（mock RPC）────────────────────────────────────

    use super::super::spec::WordCategory;
    use crate::state::SchemaInfo;

    fn with_mock<R>(f: impl FnOnce() -> R) -> R {
        let _g = crate::rpc::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("WIND_RPC_MOCK", "1") };
        crate::rpc::take_mock_calls();
        let r = f();
        unsafe { std::env::remove_var("WIND_RPC_MOCK") };
        r
    }

    /// 两个码表方案，停在第一个的「用户词库」上，分库列表已取（mock 给「成语」「hangye」）。
    fn state() -> DictManagerState {
        let st = DictManagerState::new();
        *st.schemas.borrow_mut() = vec![
            SchemaInfo {
                id: "wb".into(),
                name: "五笔".into(),
                kind: "codetable".into(),
                ..Default::default()
            },
            SchemaInfo {
                id: "zm".into(),
                name: "郑码".into(),
                kind: "codetable".into(),
                ..Default::default()
            },
        ];
        st.domain.set(super::super::state::GLOBAL_DOMAINS);
        st.sub_tab.set(0);
        // 走一遍导航（而不只是重载）：记下所在数据域，之后切子标签才不会被当成切域。
        st.on_nav_changed();
        st
    }

    fn calls_of(method: &str) -> Vec<Value> {
        crate::rpc::take_mock_calls()
            .into_iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p)
            .collect()
    }

    fn select(st: &DictManagerState, name: Option<&str>) {
        st.select_lib(name.map(String::from));
    }

    #[test]
    fn libraries_are_listed_for_the_schema() {
        with_mock(|| {
            let st = state();
            assert!(st.has_lib_group());
            let names: Vec<String> = st.libs.list.get().into_iter().map(|l| l.name).collect();
            assert_eq!(names, ["成语", "hangye"]);
            assert_eq!(st.current_category(), WordCategory::UserDict, "默认主词库");
            // 停在临时词库上中栏照样列出分库
            st.sub_tab.set(1);
            st.on_nav_changed();
            assert!(st.has_lib_group());
            // 短语域没有
            st.domain.set(super::super::state::DOMAIN_PHRASE);
            st.on_nav_changed();
            assert!(!st.has_lib_group());
        });
    }

    /// 停在别的类别上选分库：只记下，不重载表格；切回用户词库时就是它。
    #[test]
    fn selecting_a_library_on_another_tab_only_remembers() {
        with_mock(|| {
            let st = state();
            st.sub_tab.set(1);
            st.on_nav_changed();
            crate::rpc::take_mock_calls();
            select(&st, Some("成语"));
            assert!(calls_of("userDicts.entries").is_empty());
            assert_eq!(st.libs.current.get().as_deref(), Some("成语"));
            st.sub_tab.set(0);
            st.on_nav_changed();
            assert_eq!(st.current_category(), WordCategory::UserLib);
        });
    }

    #[test]
    fn library_list_is_fetched_once_per_schema() {
        with_mock(|| {
            let st = state();
            crate::rpc::take_mock_calls();
            st.reload();
            st.next_page();
            assert!(calls_of("userDicts.list").is_empty(), "翻页 / 重载不重取");
            st.reload_with_libs();
            assert_eq!(calls_of("userDicts.list"), vec![json!({"schemaId": "wb"})]);
        });
    }

    /// 选中分库：类别变 UserLib，列表走 userDicts.entries 且带分库名，删除走 removeEntry。
    #[test]
    fn selecting_a_library_switches_list_and_delete() {
        with_mock(|| {
            let st = state();
            crate::rpc::take_mock_calls();
            select(&st, Some("成语"));
            assert_eq!(st.current_category(), WordCategory::UserLib);
            let lists = calls_of("userDicts.entries");
            assert_eq!(
                lists,
                vec![json!({"schemaId": "wb", "name": "成语", "offset": 0, "limit": 100})]
            );
            assert_eq!(st.total.get(), 2);
            assert_eq!(
                st.delete_call("yzwg", "一针见血"),
                (
                    "userDicts.removeEntry",
                    json!({"schemaId": "wb", "name": "成语", "code": "yzwg", "text": "一针见血"})
                )
            );
            let ops = st.current_spec().ops;
            assert!(ops.remove && ops.add && ops.edit && ops.clear && ops.multi_select);
            assert!(!ops.import);
            assert_eq!(
                st.current_spec().rpc.add,
                "userDicts.addEntry",
                "分库的加词写分库文件"
            );
            assert!(st.current_spec().searchable);
            assert!(!st.io_entry_visible(), "分库不显示导入导出");
            assert_eq!(
                st.edit_file_target(),
                Some(json!({"kind": "user_dict", "schemaId": "wb", "name": "成语"}))
            );
            // 回主词库：一切照旧
            select(&st, None);
            assert_eq!(st.current_category(), WordCategory::UserDict);
            assert!(st.io_entry_enabled());
            assert_eq!(
                st.edit_file_target(),
                Some(json!({"schemaId": "wb", "kind": "words"}))
            );
        });
    }

    /// 移动：主词库里看得到所有分库；分库里看得到主词库与别的分库，看不到自己。
    #[test]
    fn move_targets_exclude_the_current_library() {
        with_mock(|| {
            let st = state();
            let names = |st: &DictManagerState| -> Vec<String> {
                st.move_targets().into_iter().map(|(l, _)| l).collect()
            };
            assert_eq!(names(&st), ["成语", "行业词（已停用）"]);
            select(&st, Some("成语"));
            assert_eq!(names(&st), ["主词库", "行业词（已停用）"]);
            st.sub_tab.set(1);
            st.on_nav_changed();
            assert!(st.move_targets().is_empty(), "临时词库上没有");
        });
    }

    /// 移到停用的分库：提示里分库名不带后缀，「（已停用）」放在引号外。
    #[test]
    fn moving_to_a_disabled_library_says_so_briefly() {
        with_mock(|| {
            let st = state();
            let (label, to) = st.move_targets().pop().unwrap();
            let row = super::super::spec::WordRow {
                code: "yzwg".into(),
                text: "一针见血".into(),
                ..Default::default()
            };
            st.move_rows(vec![row], &to, &label);
            let (_, msg) = st.feedback.get().unwrap();
            assert_eq!(msg, "已移动 1 条到「行业词」（已停用）");
        });
    }

    /// 主词库 → 分库：先加到分库再从主词库删；没写权重的不带权重。
    #[test]
    fn moving_adds_then_removes() {
        with_mock(|| {
            let st = state();
            let rows = vec![
                super::super::spec::WordRow {
                    code: "yzwg".into(),
                    text: "一针见血".into(),
                    weight: 5,
                    ..Default::default()
                },
                super::super::spec::WordRow {
                    code: "ggtt".into(),
                    text: "一五一十".into(),
                    ..Default::default()
                },
            ];
            crate::rpc::take_mock_calls();
            st.move_rows(rows, &MoveTarget::Lib("成语".into()), "成语");
            let calls: Vec<(String, Value)> = crate::rpc::take_mock_calls()
                .into_iter()
                .filter(|(m, _)| m == "userDicts.addEntry" || m == "dict.remove")
                .collect();
            assert_eq!(
                calls,
                vec![
                    (
                        "userDicts.addEntry".into(),
                        json!({"schemaId": "wb", "name": "成语", "code": "yzwg", "text": "一针见血", "weight": 5})
                    ),
                    ("dict.remove".into(), st.delete_call("yzwg", "一针见血").1),
                    (
                        "userDicts.addEntry".into(),
                        json!({"schemaId": "wb", "name": "成语", "code": "ggtt", "text": "一五一十"})
                    ),
                    ("dict.remove".into(), st.delete_call("ggtt", "一五一十").1),
                ]
            );
            let (_, msg) = st.feedback.get().unwrap();
            assert_eq!(msg, "已移动 2 条到「成语」");
        });
    }

    /// 加到目标失败：原条目不删，报已移几条。
    #[test]
    fn moving_stops_at_the_first_failure() {
        with_mock(|| {
            let st = state();
            let row = super::super::spec::WordRow {
                code: "yzwg".into(),
                text: "一针见血".into(),
                ..Default::default()
            };
            crate::rpc::force_mock_error("userDicts.addEntry", Some("正在外部编辑"));
            crate::rpc::take_mock_calls();
            st.move_rows(vec![row], &MoveTarget::Lib("成语".into()), "成语");
            crate::rpc::force_mock_error("userDicts.addEntry", None);
            assert!(
                !crate::rpc::take_mock_calls()
                    .iter()
                    .any(|(m, _)| m == "dict.remove"),
                "没加进去就不删"
            );
            let (_, msg) = st.feedback.get().unwrap();
            assert_eq!(msg, "已移动 0/1 条后失败：正在外部编辑");
        });
    }

    /// 分库里加词 / 改权重：写分库文件（userDicts.addEntry，带分库名）。
    #[test]
    fn adding_to_a_library_writes_the_library() {
        with_mock(|| {
            let st = state();
            select(&st, Some("成语"));
            st.open_add();
            st.edit_code.set("yzwg".into());
            st.edit_text.set("一针见血".into());
            st.edit_weight.set("5".into());
            let (calls, _) = st.save_calls().unwrap();
            assert_eq!(
                calls,
                vec![(
                    "userDicts.addEntry",
                    json!({"schemaId": "wb", "name": "成语", "code": "yzwg", "text": "一针见血", "weight": 5})
                )]
            );
        });
    }

    #[test]
    fn search_in_a_library_goes_to_the_server() {
        with_mock(|| {
            let st = state();
            select(&st, Some("成语"));
            crate::rpc::take_mock_calls();
            st.search.set("yz".into());
            st.on_search_changed();
            let p = calls_of("userDicts.entries");
            assert_eq!(p[0]["query"], json!("yz"));
            assert_eq!(p[0]["name"], json!("成语"));
        });
    }

    #[test]
    fn selection_is_remembered_per_schema() {
        with_mock(|| {
            let st = state();
            select(&st, Some("hangye"));
            assert_eq!(st.selected_lib().as_deref(), Some("hangye"));
            assert_eq!(
                st.libs.notice.get(),
                "⚠ 分库「行业词」：1 行格式不对，已跳过"
            );
            // 换到郑码：没选过，主词库
            st.domain.set(super::super::state::GLOBAL_DOMAINS + 1);
            st.on_nav_changed();
            assert_eq!(st.selected_lib(), None);
            assert_eq!(st.libs.current.get(), None);
            assert_eq!(st.libs.notice.get(), "");
            // 回五笔：还是行业词
            st.domain.set(super::super::state::GLOBAL_DOMAINS);
            st.on_nav_changed();
            assert_eq!(st.selected_lib().as_deref(), Some("hangye"));
            assert_eq!(st.libs.current.get().as_deref(), Some("hangye"));
            assert_eq!(st.current_category(), WordCategory::UserLib);
        });
    }

    /// 记着的分库没了（外面删了文件）：刷新后回到主词库。
    #[test]
    fn vanished_library_falls_back_to_main() {
        with_mock(|| {
            let st = state();
            st.libs
                .memory
                .borrow_mut()
                .insert("wb".into(), "已删".into());
            st.reload_with_libs();
            assert_eq!(st.selected_lib(), None);
            assert_eq!(st.current_category(), WordCategory::UserDict);
        });
    }

    #[test]
    fn toggle_failure_reports_and_reverts() {
        with_mock(|| {
            let st = state();
            crate::rpc::take_mock_calls();
            assert!(st.set_lib_enabled("成语", false));
            assert_eq!(
                calls_of("userDicts.setEnabled"),
                vec![json!({"schemaId": "wb", "name": "成语", "enabled": false})]
            );
            let (_, msg) = st.feedback.get().expect("开关后有提示");
            assert_eq!(msg, "已停用「成语」");
            crate::rpc::force_mock_error("userDicts.setEnabled", Some("只读模式"));
            assert!(!st.set_lib_enabled("成语", false));
            crate::rpc::force_mock_error("userDicts.setEnabled", None);
            let (_, msg) = st.feedback.get().expect("应有提示");
            assert_eq!(msg, "切换失败：只读模式");
            assert!(st.libs.list.get()[0].enabled, "列表按 core 的状态");
        });
    }

    #[test]
    fn create_validates_then_calls_core() {
        with_mock(|| {
            let st = state();
            st.open_lib_create();
            assert!(st.libs.prompt_visible.get());
            assert!(!st.lib_prompt_ready(), "空名字不可提交");
            st.libs.name.set("成语".into());
            st.on_lib_name_changed();
            assert_eq!(st.libs.name_error.get(), "已有同名分库");
            assert!(!st.lib_prompt_ready());
            st.libs.name.set(" 网络词 ".into());
            st.on_lib_name_changed();
            assert!(st.lib_prompt_ready(), "首尾空格自动去掉");
            crate::rpc::take_mock_calls();
            st.submit_lib_prompt();
            assert_eq!(
                calls_of("userDicts.create"),
                vec![json!({"schemaId": "wb", "name": "网络词"})]
            );
            assert!(!st.libs.prompt_visible.get());
            let (_, msg) = st.feedback.get().unwrap();
            assert_eq!(msg, "已新建分库「网络词」");
        });
    }

    #[test]
    fn rename_follows_the_selection() {
        with_mock(|| {
            let st = state();
            select(&st, Some("成语"));
            st.open_lib_rename("成语");
            assert_eq!(st.libs.name.get(), "成语");
            st.libs.name.set("熟语".into());
            crate::rpc::take_mock_calls();
            st.submit_lib_prompt();
            assert_eq!(
                calls_of("userDicts.rename"),
                vec![json!({"schemaId": "wb", "name": "成语", "newName": "熟语"})]
            );
            // mock 的列表里没有「熟语」，刷新后回落主词库；记忆里先改了名，这条路是通的。
            assert_eq!(st.selected_lib(), None);
        });
    }

    #[test]
    fn delete_needs_confirmation() {
        with_mock(|| {
            let st = state();
            st.request_delete_user_lib("hangye", "行业词");
            assert!(st.confirm_visible.get());
            assert_eq!(
                st.confirm_msg.get(),
                "确定删除分库「行业词」？会删除文件，不能恢复。"
            );
            crate::rpc::take_mock_calls();
            st.confirm_pending();
            assert_eq!(
                calls_of("userDicts.delete"),
                vec![json!({"schemaId": "wb", "name": "hangye"})]
            );
        });
    }

    fn temp_file(tag: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wind_ulibs_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("网络词.dict.yaml");
        std::fs::write(&p, body).unwrap();
        p
    }

    /// 词库文件：传路径给 core 识别。
    #[test]
    fn import_dict_file_passes_path() {
        with_mock(|| {
            let st = state();
            let p = temp_file("path", "---\nname: x\n...\n工\ta\n");
            st.open_lib_import(&p);
            assert_eq!(st.libs.name.get(), "网络词");
            assert!(st.libs.importing.get());
            assert_eq!(st.libs.format_note.get(), "");
            crate::rpc::take_mock_calls();
            st.submit_lib_prompt();
            assert_eq!(
                calls_of("userDicts.importAs"),
                vec![json!({"schemaId": "wb", "name": "网络词", "path": p.to_string_lossy()})]
            );
            let (_, msg) = st.feedback.get().unwrap();
            assert_eq!(msg, "已导入 2 条到分库「网络词」");
            let _ = std::fs::remove_dir_all(p.parent().unwrap());
        });
    }

    /// 纯词列表：先 dict.encodeWords 出码，拼 TSV 以 content 传。
    #[test]
    fn import_word_list_encodes_first() {
        with_mock(|| {
            let st = state();
            let p = temp_file("words", "中国\n计算机\n");
            st.open_lib_import(&p);
            assert_eq!(st.libs.format_note.get(), "纯词列表 · 2 个词，按本方案出码");
            crate::rpc::take_mock_calls();
            st.submit_lib_prompt();
            let calls = crate::rpc::take_mock_calls();
            let names: Vec<&str> = calls.iter().map(|(m, _)| m.as_str()).collect();
            let enc = names.iter().position(|m| *m == "dict.encodeWords").unwrap();
            let imp = names
                .iter()
                .position(|m| *m == "userDicts.importAs")
                .unwrap();
            assert!(enc < imp);
            let params = &calls[imp].1;
            assert!(params.get("path").is_none());
            let tsv = params["content"].as_str().unwrap();
            assert!(tsv.contains("\t中国\t1200\n"), "{tsv}");
            let _ = std::fs::remove_dir_all(p.parent().unwrap());
        });
    }

    /// 打开目录：data.locate 带方案 id。
    #[test]
    fn open_dir_locates_the_schema_data_dir() {
        with_mock(|| {
            let st = state();
            crate::rpc::take_mock_calls();
            st.open_libs_dir();
            assert_eq!(calls_of("data.locate"), vec![json!({"schemaId": "wb"})]);
        });
    }

    #[test]
    fn edit_lib_file_opens_the_library_file() {
        with_mock(|| {
            let st = state();
            crate::rpc::take_mock_calls();
            st.edit_lib_file("成语");
            assert_eq!(
                calls_of("data.beginEdit"),
                vec![json!({"kind": "user_dict", "schemaId": "wb", "name": "成语"})]
            );
            assert_eq!(st.edit_bars.get()[0].file, "wb/dicts/成语.dict.yaml");
        });
    }

    // ── 拼音族折叠域：开关对每个成员方案生效 ─────────────────

    /// 拼音域（全拼 py + 双拼 sp，代表 py），停在用户词库上。`bad` 打头的方案 mock 开关会失败。
    fn pinyin_state(sp: &str) -> DictManagerState {
        let st = DictManagerState::new();
        *st.schemas.borrow_mut() = vec![
            SchemaInfo {
                id: "py".into(),
                name: "全拼".into(),
                kind: "pinyin".into(),
                scheme: "full".into(),
                ..Default::default()
            },
            SchemaInfo {
                id: sp.into(),
                name: "双拼".into(),
                kind: "pinyin".into(),
                scheme: "shuangpin".into(),
                ..Default::default()
            },
        ];
        st.domain.set(super::super::state::GLOBAL_DOMAINS);
        st.sub_tab.set(0);
        st
    }

    #[test]
    fn folded_domain_toggles_every_member() {
        with_mock(|| {
            let st = pinyin_state("sp");
            st.reload();
            crate::rpc::take_mock_calls();
            assert!(st.set_lib_enabled("成语", false));
            assert_eq!(
                calls_of("userDicts.setEnabled"),
                vec![
                    json!({"schemaId": "py", "name": "成语", "enabled": false}),
                    json!({"schemaId": "sp", "name": "成语", "enabled": false}),
                ]
            );
        });
    }

    #[test]
    fn folded_domain_rolls_back_on_partial_failure() {
        with_mock(|| {
            let st = pinyin_state("bad_sp");
            st.reload();
            crate::rpc::take_mock_calls();
            assert!(!st.set_lib_enabled("成语", false));
            assert_eq!(
                calls_of("userDicts.setEnabled"),
                vec![
                    json!({"schemaId": "py", "name": "成语", "enabled": false}),
                    json!({"schemaId": "bad_sp", "name": "成语", "enabled": false}),
                    json!({"schemaId": "py", "name": "成语", "enabled": true}),
                ],
                "已改的成员改回去"
            );
            let (_, msg) = st.feedback.get().expect("应有提示");
            assert!(msg.starts_with("切换失败："), "{msg}");
        });
    }

    /// 显示取活动方案那一份开关：活动方案是双拼就按双拼列，不在域内就按代表方案。
    #[test]
    fn folded_domain_shows_the_active_members_switches() {
        with_mock(|| {
            let st = pinyin_state("sp");
            crate::rpc::force_mock_reply("schema.active", Some(json!({"id": "sp"})));
            crate::rpc::take_mock_calls();
            st.reload();
            assert_eq!(calls_of("userDicts.list"), vec![json!({"schemaId": "sp"})]);
            crate::rpc::force_mock_reply("schema.active", Some(json!({"id": "wubi86"})));
            st.reload_with_libs();
            assert_eq!(calls_of("userDicts.list"), vec![json!({"schemaId": "py"})]);
            crate::rpc::force_mock_reply("schema.active", None);
            // 条目、删除仍按代表方案（数据共用）
            select(&st, Some("成语"));
            assert_eq!(st.delete_call("a", "b").1["schemaId"], json!("py"));
        });
    }

    /// 非折叠域：只调自己，不问活动方案。
    #[test]
    fn plain_domain_toggles_only_itself() {
        with_mock(|| {
            let st = state();
            crate::rpc::take_mock_calls();
            st.reload_with_libs();
            st.set_lib_enabled("成语", false);
            let calls = crate::rpc::take_mock_calls();
            assert!(calls.iter().all(|(m, _)| m != "schema.active"));
            let sets: Vec<&Value> = calls
                .iter()
                .filter(|(m, _)| m == "userDicts.setEnabled")
                .map(|(_, p)| p)
                .collect();
            assert_eq!(sets.len(), 1);
        });
    }
}
