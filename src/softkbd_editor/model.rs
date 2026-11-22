//! 软键盘编辑器的工作副本：core 视图（`softkeyboard.get`）+ 尚未提交的操作队列。
//!
//! 对话框里的每次改动同时做两件事：改本地副本（界面当场看到结果），往 `pending` 追加一条
//! core 认识的操作（`wind_softkeyboard::admin::Op` 的 JSON 形态）。「应用设置」时整批发给
//! `softkeyboard.apply`，core 按同样的语义稀疏写用户层，再回一份新视图替换本地副本。
//!
//! ★ 本地副本只为「当场看到」服务，**不是写入的依据**：写什么、补丁该不该消失，全由 core
//! 按真实的下层判定（见 core 仓 `docs/design/soft-keyboard.md` §2.2.1）。这边不复刻那套
//! 稀疏规则，复刻一份就是第二个真相源。

use serde::Deserialize;
use serde_json::{Value, json};

/// 一个键位。`None` = 空键位。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyCell {
    pub slot: String,
    pub base: Option<String>,
    pub shift: Option<String>,
    pub default_base: Option<String>,
    pub default_shift: Option<String>,
}

impl KeyCell {
    pub fn get(&self, shift: bool) -> Option<&str> {
        if shift { &self.shift } else { &self.base }.as_deref()
    }

    pub fn default_of(&self, shift: bool) -> Option<&str> {
        if shift {
            &self.default_shift
        } else {
            &self.default_base
        }
        .as_deref()
    }

    /// 与默认值不同（界面上标「已修改」）。按值比而非按「有没有补丁」：用户手写了一条与
    /// 默认相同的补丁，界面不该说它改过。
    pub fn modified(&self) -> bool {
        self.base != self.default_base || self.shift != self.default_shift
    }
}

/// 一个面。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageModel {
    pub id: String,
    pub name: String,
    pub default_name: String,
    pub user_created: bool,
    pub send_keys: bool,
    pub keys: Vec<KeyCell>,
}

impl PageModel {
    pub fn key(&self, slot: &str) -> Option<&KeyCell> {
        self.keys.iter().find(|k| k.slot == slot)
    }

    /// 出厂面被改过（名字或任何一个键），可「恢复默认」。
    pub fn modified(&self) -> bool {
        self.name != self.default_name || self.keys.iter().any(KeyCell::modified)
    }
}

#[derive(Deserialize)]
struct ViewReply {
    pages: Vec<PageModel>,
}

/// 解析 `softkeyboard.get` / `softkeyboard.apply` 的回复。
pub fn parse_view(v: &Value) -> Result<Vec<PageModel>, String> {
    ViewReply::deserialize(v)
        .map(|r| r.pages)
        .map_err(|e| format!("软键盘数据格式不对：{e}"))
}

/// 工作副本。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Model {
    pub pages: Vec<PageModel>,
    /// 尚未提交的操作，按发生顺序。
    pub pending: Vec<Value>,
}

impl Model {
    pub fn from_view(pages: Vec<PageModel>) -> Self {
        Self {
            pages,
            pending: Vec::new(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        !self.pending.is_empty()
    }

    /// 设一个键。与当前值相同则什么都不做（选中键时回填输入框也会走到这里）。
    ///
    /// 连续改同一个键只留最后一条操作：输入框每敲一个字都会触发，不合并的话改一个词要
    /// 攒十几条。
    pub fn set_key(&mut self, page: usize, slot: &str, shift: bool, value: Option<String>) {
        let value = value.filter(|s| !s.is_empty());
        let Some(p) = self.pages.get_mut(page) else {
            return;
        };
        let Some(k) = p.keys.iter_mut().find(|k| k.slot == slot) else {
            return;
        };
        let cur = if shift { &mut k.shift } else { &mut k.base };
        if *cur == value {
            return;
        }
        *cur = value.clone();
        let op =
            json!({ "op": "setKey", "page": p.id, "slot": slot, "shift": shift, "value": value });
        self.push_coalescing(op);
    }

    /// 键恢复默认（两层一起）。
    pub fn reset_key(&mut self, page: usize, slot: &str) {
        let Some(p) = self.pages.get_mut(page) else {
            return;
        };
        let Some(k) = p.keys.iter_mut().find(|k| k.slot == slot) else {
            return;
        };
        let id = p.id.clone();
        let mut ops = Vec::new();
        for shift in [false, true] {
            let def = k.default_of(shift).map(str::to_string);
            let cur = if shift { &mut k.shift } else { &mut k.base };
            if *cur != def {
                *cur = def;
                ops.push(json!({ "op": "resetKey", "page": id, "slot": slot, "shift": shift }));
            }
        }
        for op in ops {
            self.push_coalescing(op);
        }
    }

    /// 新建一面，返回它的下标。`copy_from` 给了就复制那一面当前的全部键。新面恒是符号面
    /// （与 core 同：不继承键盘面属性），可编辑。
    pub fn create_page(&mut self, name: &str, copy_from: Option<usize>) -> usize {
        let id = self.fresh_id();
        let src = copy_from.and_then(|i| self.pages.get(i)).cloned();
        let keys = match &src {
            Some(s) => s
                .keys
                .iter()
                .map(|k| KeyCell {
                    default_base: None,
                    default_shift: None,
                    ..k.clone()
                })
                .collect(),
            None => all_slot_names()
                .iter()
                .map(|s| KeyCell {
                    slot: (*s).to_string(),
                    base: None,
                    shift: None,
                    default_base: None,
                    default_shift: None,
                })
                .collect(),
        };
        self.pending.push(json!({
            "op": "createPage",
            "id": id,
            "name": name,
            "copyFrom": src.as_ref().map(|s| s.id.clone()),
        }));
        self.pages.push(PageModel {
            id,
            name: name.to_string(),
            default_name: name.to_string(),
            user_created: true,
            send_keys: false,
            keys,
        });
        self.pages.len() - 1
    }

    pub fn rename_page(&mut self, page: usize, name: &str) {
        let Some(p) = self.pages.get_mut(page) else {
            return;
        };
        if p.name == name {
            return;
        }
        p.name = name.to_string();
        if p.user_created {
            // 新建面的「默认名」就是它当前的名字：它没有出厂值可回。
            p.default_name = name.to_string();
        }
        let op = json!({ "op": "renamePage", "page": p.id, "name": name });
        self.push_coalescing(op);
    }

    /// 删除用户新建的面；出厂面返回 `false`（它们只能恢复默认）。
    pub fn delete_page(&mut self, page: usize) -> bool {
        match self.pages.get(page) {
            Some(p) if p.user_created => {
                let id = p.id.clone();
                self.pages.remove(page);
                self.pending.push(json!({ "op": "deletePage", "page": id }));
                true
            }
            _ => false,
        }
    }

    /// 出厂面恢复默认；用户新建的面返回 `false`。
    pub fn reset_page(&mut self, page: usize) -> bool {
        match self.pages.get_mut(page) {
            Some(p) if !p.user_created => {
                p.name = p.default_name.clone();
                for k in &mut p.keys {
                    k.base = k.default_base.clone();
                    k.shift = k.default_shift.clone();
                }
                let op = json!({ "op": "resetPage", "page": p.id });
                self.pending.push(op);
                true
            }
            _ => false,
        }
    }

    /// 同一目标的连续操作只留最后一条（目标 = op 类型之外的定位字段：面 + 键位 + 层）。
    fn push_coalescing(&mut self, op: Value) {
        if let Some(last) = self.pending.last()
            && same_target(last, &op)
        {
            *self.pending.last_mut().expect("刚判过非空") = op;
            return;
        }
        self.pending.push(op);
    }

    /// 新面的 id：`user1`、`user2`……跳过已有的（含本次已删、尚未提交的也不复用，
    /// 免得同一批里「删 user1 → 建 user1」让 core 判成同一面）。
    fn fresh_id(&self) -> String {
        let taken = |id: &str| {
            self.pages.iter().any(|p| p.id == id)
                || self
                    .pending
                    .iter()
                    .any(|op| op["page"] == id || op["id"] == id)
        };
        (1..)
            .map(|n| format!("user{n}"))
            .find(|id| !taken(id))
            .expect("无限序列")
    }
}

fn same_target(a: &Value, b: &Value) -> bool {
    let kind = |v: &Value| match v["op"].as_str() {
        Some("setKey" | "resetKey") => "key",
        Some("renamePage") => "rename",
        _ => "",
    };
    !kind(a).is_empty()
        && kind(a) == kind(b)
        && a["page"] == b["page"]
        && a["slot"] == b["slot"]
        && a["shift"] == b["shift"]
}

/// 软键盘的 47 个键位，按键盘行列顺序。**与 core 的 `wind_softkeyboard::KEY_ROWS` 同构**——
/// 设置端运行时不依赖那个 crate，故手抄一份；漂移由 `slot_rows_match_core` 对账。
pub const SLOT_ROWS: [&[&str]; 4] = [
    &[
        "grave", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "minus", "equal",
    ],
    &[
        "q",
        "w",
        "e",
        "r",
        "t",
        "y",
        "u",
        "i",
        "o",
        "p",
        "lbracket",
        "rbracket",
        "backslash",
    ],
    &[
        "a",
        "s",
        "d",
        "f",
        "g",
        "h",
        "j",
        "k",
        "l",
        "semicolon",
        "quote",
    ],
    &[
        "z", "x", "c", "v", "b", "n", "m", "comma", "period", "slash",
    ],
];

pub fn all_slot_names() -> Vec<&'static str> {
    SLOT_ROWS.iter().flat_map(|r| r.iter().copied()).collect()
}

/// 键帽角标：物理键上印的字（`lbracket` → `[`）。
pub fn slot_caption(slot: &str) -> String {
    match slot {
        "grave" => "`",
        "minus" => "-",
        "equal" => "=",
        "lbracket" => "[",
        "rbracket" => "]",
        "backslash" => "\\",
        "semicolon" => ";",
        "quote" => "'",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        s => return s.to_uppercase(),
    }
    .to_string()
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
