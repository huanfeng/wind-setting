//! 方案级会话态按键表（`[session_actions]`）编辑对话框。
//!
//! # 为什么复用全局那份行编辑器
//!
//! 键名值域与动词值域两层**完全相同**——core 侧 `Schema::session_actions` 与
//! `keys.session_actions` 是同一个 `SessionAction` 值域。再写一份就是两处慢慢漂移：
//! core 加一个动词时漏改一处，表现是「设置页里选不到这个动作」，而用户手写配置能用，
//! 完全查不出所以然。故本模块只包一层，行编辑器整个来自
//! [`super::session_actions::SessionActionFields`]。
//!
//! 与 [`super::schema_key_actions`] 的关系是对称的：那张管无会话态的引导键
//! （`[key_actions]`），本张管有会话态的翻页/选词/取消（`[session_actions]`）。两张表
//! 分开而不是合成一张带状态维度的表，判据见 core 仓 `docs/design/session-key-actions.md` §2。
//!
//! # 落盘
//!
//! 与双拼布局、码表配置、方案级 `[key_actions]` 写进**同一份 cfg**（`schema.getConfig`
//! 的结果），由 `schema_manager::commit_settings_edits` 统一回传 `schema.saveConfig`。
//! core 那边是「整份 cfg 与方案文件 diff、结果全量重写 override」，分两次提交会互相覆盖。
//!
//! # 逐键合并与「禁用」
//!
//! 方案级与全局是**逐键合并**：方案只写想改的键，其余继承全局。故「本方案禁用某个全局
//! 绑定」必须显式选「输入中禁用该键」（`none`）——**删掉这一行只是回到继承全局**，不是
//! 禁用。core 侧 `merge_toml` 只能新增/覆盖、无法表达删除，这条语义在两端是一致的。
//! 见 core 仓 `docs/design/key-resolver-unification.md`。

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Map, Value, json};
use windui::prelude::*;

use super::session_actions::SessionActionFields;

/// 方案级会话态按键表的工作态。
#[derive(Clone, Default)]
pub struct SchemaSessionActionFields {
    inner: SessionActionFields,
    /// 打开对话框时的内容快照，供 [`Self::is_dirty`] 比对。
    ///
    /// `Rc` 是必须的：本结构体被 `Clone` 进各处闭包，裸 `RefCell` 会让每份副本各存一份
    /// 快照，行内改动写在一份、脏判定读另一份，表现是「改了却不保存」。
    at_open: Rc<RefCell<Map<String, Value>>>,
}

impl SchemaSessionActionFields {
    /// 从方案配置播种。
    pub fn seed(&self, cfg: &Value) {
        let map = cfg
            .get("session_actions")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        self.inner.seed(&map);
        // ★ 快照取 `to_map()` 的结果而**不是**上面那个原始 map：`seed` 会规范化（按键名
        // 排序、把未知动词补进下拉），两者未必逐字节相等。存原始 map 的话，只要方案文件
        // 里的键序与规范序不同，对话框一打开就是 dirty —— 用户什么都没动，保存却被触发。
        *self.at_open.borrow_mut() = self.inner.to_map();
    }

    /// 内容是否较打开时有变化。
    pub fn is_dirty(&self) -> bool {
        self.inner.to_map() != *self.at_open.borrow()
    }

    /// 写进方案配置。
    ///
    /// 空表写 `{}` 而非 null：core 的 `json_diff` 拿方案文件基线比对，空对象与基线的空表
    /// 相等即不落 override；写 null 会让 `json_to_toml` 跳过，**清空操作就丢了**——用户
    /// 删光所有绑定后 override 里仍留着旧表。与 `schema_key_actions::apply` 同一条。
    pub fn apply(&self, cfg: &mut Value) {
        if !cfg.is_object() {
            *cfg = json!({});
        }
        if let Some(o) = cfg.as_object_mut() {
            o.insert(
                "session_actions".to_string(),
                Value::Object(self.inner.to_map()),
            );
        }
    }

    /// 行摘要（「未设置」/「N 项」），显示在设置页那一行的右侧。
    pub fn summary(&self) -> String {
        let n = self.inner.to_map().len();
        if n == 0 {
            "未设置".to_string()
        } else {
            format!("{n} 项")
        }
    }

    /// 对话框主体，直接来自复用的行编辑器。
    pub fn build_body(&self) -> (Element, Vec<Element>) {
        self.inner.build_body()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(pairs: &[(&str, &str)]) -> Value {
        let mut m = Map::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), Value::String(v.to_string()));
        }
        json!({ "session_actions": Value::Object(m) })
    }

    /// 播种 → 写回应当往返一致，且不因排序差异误判为脏。
    #[test]
    fn seed_then_apply_roundtrips_and_is_not_dirty() {
        let f = SchemaSessionActionFields::default();
        // 故意用非字典序，验证 `seed` 规范化后不会被当成用户改动。
        let src = cfg_with(&[("minus", "page_prev"), ("equal", "page_next")]);
        f.seed(&src);
        assert!(
            !f.is_dirty(),
            "刚播种就该是干净的，否则打开设置页即触发保存"
        );

        let mut out = json!({});
        f.apply(&mut out);
        let got = out
            .get("session_actions")
            .and_then(|v| v.as_object())
            .cloned();
        assert_eq!(got.map(|m| m.len()), Some(2), "两条绑定应原样写回：{out:?}");
    }

    /// 显式 `none` 是一条**绑定**，必须原样往返——它是「本方案禁用该键」的唯一载体。
    /// 当成空值丢掉的话，落盘后 core 侧就回落全局，用户配的禁用完全失效。
    #[test]
    fn explicit_none_survives_roundtrip() {
        let f = SchemaSessionActionFields::default();
        f.seed(&cfg_with(&[("minus", "none")]));
        let mut out = json!({});
        f.apply(&mut out);
        assert_eq!(
            out["session_actions"]["minus"],
            Value::String("none".to_string()),
            "显式 none 不能在往返中丢失"
        );
    }

    /// 空表写 `{}` 而非 null，否则 core 侧 `json_to_toml` 跳过，清空操作丢失。
    #[test]
    fn empty_table_writes_object_not_null() {
        let f = SchemaSessionActionFields::default();
        f.seed(&json!({}));
        let mut out = json!({});
        f.apply(&mut out);
        assert_eq!(
            out.get("session_actions"),
            Some(&Value::Object(Map::new())),
            "空表必须写成 {{}}，写 null 会让清空操作丢失"
        );
    }

    /// 摘要文案：0 条与 N 条。
    #[test]
    fn summary_reports_row_count() {
        let f = SchemaSessionActionFields::default();
        f.seed(&json!({}));
        assert_eq!(f.summary(), "未设置");
        f.seed(&cfg_with(&[("minus", "page_prev")]));
        assert_eq!(f.summary(), "1 项");
    }
}
