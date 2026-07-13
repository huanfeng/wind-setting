//! 备份还原（高级页卡片 + 还原确认对话框）。
//!
//! 组合 `backup.create` / `backup.inspect` / `backup.restore` 三个 RPC（P4 契约）：
//! 创建备份可选是否包含统计/界面状态；还原前经 inspect 摘要确认，
//! 可勾选还原范围（sections）并二选一合并/替换策略。

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use windui::core::EventCtx;
use windui::prelude::*;

use crate::pages::dict::reactive::Feedback;
use crate::rpc;
use crate::widgets::{flat_panel, section, setting_row};

/// 备份还原状态（Signal 为 Copy，可直接 Clone 进闭包）。
#[derive(Clone)]
pub struct BackupState {
    /// 创建选项：是否包含统计 / 界面状态（本机相关，默认不含）。
    pub include_stats: Signal<bool>,
    pub include_state: Signal<bool>,
    /// 还原确认对话框。
    pub restore_confirm_visible: Signal<bool>,
    pub restore_summary_msg: Signal<String>,
    /// 还原域勾选（打开对话框时重置为全选）。
    pub sec_config: Signal<bool>,
    pub sec_data: Signal<bool>,
    pub sec_stats: Signal<bool>,
    pub sec_schemas: Signal<bool>,
    pub sec_themes: Signal<bool>,
    pub sec_state: Signal<bool>,
    /// 待还原的备份包路径（inspect 确认后执行）。
    pending_restore_path: Rc<RefCell<Option<std::path::PathBuf>>>,
    pub feedback: Feedback,
}

impl Default for BackupState {
    fn default() -> Self {
        Self::new()
    }
}

impl BackupState {
    pub fn new() -> Self {
        Self {
            include_stats: signal(false),
            include_state: signal(false),
            restore_confirm_visible: signal(false),
            restore_summary_msg: signal(String::new()),
            sec_config: signal(true),
            sec_data: signal(true),
            sec_stats: signal(true),
            sec_schemas: signal(true),
            sec_themes: signal(true),
            sec_state: signal(true),
            pending_restore_path: Rc::new(RefCell::new(None)),
            feedback: signal(None),
        }
    }

    fn notify(&self, kind: ToastKind, msg: impl Into<String>) {
        self.feedback.set(Some((kind, msg.into())));
    }

    /// 创建整机备份（save 对话框异步请求，禁同步弹框，见 dict::do_export_phrases 注释）。
    pub fn do_create(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        ctx.request_save_file(
            PickDialog::new()
                .title("创建备份")
                .file_name(&format!("windinput-backup-{stamp}.zip"))
                .filter("备份包", &["zip"]),
            move |path| {
                let Some(path) = path else { return };
                match rpc::call_core(
                    "backup.create",
                    &json!({
                        "path": path.to_string_lossy(),
                        "includeStats": state.include_stats.get(),
                        "includeState": state.include_state.get(),
                    }),
                ) {
                    Ok(v) => {
                        let n = v
                            .get("manifest")
                            .and_then(|m| m.get("contents"))
                            .and_then(|c| c.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
                        state.notify(ToastKind::Success, format!("备份完成（{n} 个条目）"));
                    }
                    Err(e) => state.notify(ToastKind::Error, format!("备份失败: {e}")),
                }
            },
        );
    }

    /// 从备份还原：选 zip → `backup.inspect` 摘要 → 确认对话框（范围 + 策略）。
    pub fn do_restore(&self, ctx: &mut EventCtx) {
        let state = self.clone();
        ctx.request_pick_file(
            PickDialog::new()
                .title("从备份还原")
                .filter("备份包", &["zip"]),
            move |path| {
                let Some(path) = path else { return };
                match rpc::call_core("backup.inspect", &json!({ "path": path.to_string_lossy() })) {
                    Ok(v) => {
                        let kind = v
                            .get("manifest")
                            .and_then(|m| m.get("kind"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        if kind != "backup" {
                            state.notify(
                                ToastKind::Error,
                                "该文件不是整机备份包（方案包请到方案页导入）",
                            );
                            return;
                        }
                        state
                            .restore_summary_msg
                            .set(Self::format_inspect_summary(&v));
                        // 每次打开重置为全选。
                        for s in [
                            state.sec_config,
                            state.sec_data,
                            state.sec_stats,
                            state.sec_schemas,
                            state.sec_themes,
                            state.sec_state,
                        ] {
                            s.set(true);
                        }
                        *state.pending_restore_path.borrow_mut() = Some(path);
                        state.restore_confirm_visible.set(true);
                    }
                    Err(e) => state.notify(ToastKind::Error, format!("读取备份失败: {e}")),
                }
            },
        );
    }

    /// inspect 响应 → 摘要正文（纯函数，便于单测）。
    fn format_inspect_summary(v: &Value) -> String {
        let m = v.get("manifest");
        let s = |k: &str| {
            m.and_then(|m| m.get(k))
                .and_then(|x| x.as_str())
                .unwrap_or("未知")
                .to_string()
        };
        let mut config = 0usize;
        let mut data = 0usize;
        let mut schemas = 0usize;
        let mut themes = 0usize;
        let mut stats = 0usize;
        let mut state_n = 0usize;
        if let Some(contents) = m.and_then(|m| m.get("contents")).and_then(|c| c.as_array()) {
            for e in contents {
                match e.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "config" => config += 1,
                    "dict" | "temp" | "phrase" | "freq" | "shadow" => data += 1,
                    "schema_file" => schemas += 1,
                    "theme_file" => themes += 1,
                    "stats" | "stats_meta" => stats += 1,
                    "state" => state_n += 1,
                    _ => {}
                }
            }
        }
        format!(
            "来源 v{} · {} · 创建于 {}\n配置 {} · 数据 {} · 方案文件 {} · 主题文件 {} · 统计 {} · 界面状态 {}\n\n\
             合并保留本地已有内容；替换以备份为准覆盖。",
            s("app_version"),
            s("platform"),
            s("created_at"),
            config,
            data,
            schemas,
            themes,
            stats,
            state_n,
        )
    }

    /// 勾选 → RPC sections 参数；全选返回 None（还原全部）。
    fn selected_sections(&self) -> Option<Vec<String>> {
        let picks = [
            (self.sec_config.get(), &["config"][..]),
            (
                self.sec_data.get(),
                &["dict", "temp", "phrase", "freq", "shadow"][..],
            ),
            (self.sec_stats.get(), &["stats"][..]),
            (self.sec_schemas.get(), &["schemas"][..]),
            (self.sec_themes.get(), &["themes"][..]),
            (self.sec_state.get(), &["state"][..]),
        ];
        if picks.iter().all(|(on, _)| *on) {
            return None;
        }
        Some(
            picks
                .iter()
                .filter(|(on, _)| *on)
                .flat_map(|(_, ss)| ss.iter().map(|s| s.to_string()))
                .collect(),
        )
    }

    /// 确认还原（strategy: "merge" | "replace"）。
    pub fn confirm_restore(&self, strategy: &str) {
        self.restore_confirm_visible.set(false);
        let Some(path) = self.pending_restore_path.borrow_mut().take() else {
            return;
        };
        let mut params = json!({ "path": path.to_string_lossy(), "strategy": strategy });
        if let Some(sections) = self.selected_sections() {
            if sections.is_empty() {
                self.notify(ToastKind::Error, "未勾选任何还原范围");
                return;
            }
            params["sections"] = json!(sections);
        }
        match rpc::call_core("backup.restore", &params) {
            Ok(v) => {
                let n = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0)
                };
                let (restored, conflicts) = (n("restored"), n("conflicts"));
                let msg = if conflicts > 0 {
                    format!("已还原 {restored} 项（跳过已存在 {conflicts} 项），部分设置重启后生效")
                } else {
                    format!("已还原 {restored} 项，部分设置重启后生效")
                };
                self.notify(ToastKind::Success, msg);
            }
            Err(e) => self.notify(ToastKind::Error, format!("还原失败: {e}")),
        }
    }

    pub fn cancel_restore(&self) {
        *self.pending_restore_path.borrow_mut() = None;
        self.restore_confirm_visible.set(false);
    }
}

/// 高级页「备份与还原」卡片：创建（含两个可选项）+ 还原入口。
pub fn build_backup_section(state: &BackupState) -> Element {
    let m_create = state.clone();
    let m_restore = state.clone();

    let create_ctl = Element::row()
        .cross(Align::Center)
        .spacing(10)
        .child(
            Element::checkbox("包含统计", state.include_stats)
                .small()
                .fg_role(Role::Text),
        )
        .child(
            Element::checkbox("包含界面状态", state.include_state)
                .small()
                .fg_role(Role::Text),
        )
        .child(
            Element::button("创建")
                .small()
                .outline_soft()
                .on_click(move |ctx| m_create.do_create(ctx)),
        );

    let restore_ctl = Element::button("还原")
        .small()
        .outline_soft()
        .on_click(move |ctx| m_restore.do_restore(ctx));

    section(
        "备份与还原",
        vec![
            setting_row(
                "创建备份",
                Some("打包配置、词库数据、用户方案与主题为 zip 备份包"),
                create_ctl,
            ),
            setting_row(
                "从备份还原",
                Some("从备份包恢复，可选还原范围与合并/替换策略"),
                restore_ctl,
            ),
            crate::pages::dict::reactive::toast_sink(state.feedback),
        ],
    )
}

/// 还原确认对话框：inspect 摘要 + 还原范围勾选 + 合并（默认）/ 替换（danger）。
pub fn build_backup_restore_dialog(state: &BackupState) -> Element {
    let merge = state.clone();
    let replace = state.clone();
    let cancel = state.clone();

    let checks_row = |items: Vec<(&'static str, Signal<bool>)>| {
        let mut row = Element::row()
            .width_match()
            .cross(Align::Center)
            .spacing(12);
        for (label, sig) in items {
            row = row.child(Element::checkbox(label, sig).small().fg_role(Role::Text));
        }
        row
    };

    let content = flat_panel()
        .width(440)
        .padding(20)
        .spacing(14)
        .child(
            Element::label("从备份还原")
                .font_size(15.0)
                .font_weight(600)
                .fg_role(Role::Text)
                .height(24)
                .width_match(),
        )
        .child(Element::divider())
        .child(
            Element::label_rc(state.restore_summary_msg)
                .font_size(13.0)
                .fg_role(Role::Text)
                .width_match()
                .max_lines(6),
        )
        .child(
            Element::label("还原范围")
                .font_size(12.5)
                .fg_role(Role::TextMuted)
                .width_match(),
        )
        .child(checks_row(vec![
            ("配置", state.sec_config),
            ("词库数据", state.sec_data),
            ("统计", state.sec_stats),
        ]))
        .child(checks_row(vec![
            ("用户方案", state.sec_schemas),
            ("主题", state.sec_themes),
            ("界面状态", state.sec_state),
        ]))
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(Element::leaf().weight(1.0))
                .child(
                    Element::button("取消")
                        .small()
                        .outline_soft()
                        .on_click(move |_| cancel.cancel_restore()),
                )
                .child(
                    Element::button("替换还原")
                        .small()
                        .danger()
                        .on_click(move |_| replace.confirm_restore("replace")),
                )
                .child(
                    Element::button("合并还原")
                        .small()
                        .on_click(move |_| merge.confirm_restore("merge")),
                ),
        );
    Element::dialog(state.restore_confirm_visible, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_summary_counts_types() {
        let v = json!({ "manifest": {
            "app_version": "1.0.0", "platform": "windows", "created_at": "2026-07-13",
            "contents": [
                { "type": "config", "path": "config/config.toml" },
                { "type": "dict", "path": "userdata/user_words/wb.wdict" },
                { "type": "freq", "path": "userdata/freq/wb.jsonl" },
                { "type": "schema_file", "path": "schemas/my.schema.toml" },
                { "type": "stats", "path": "userdata/stats.jsonl" },
                { "type": "stats_meta", "path": "userdata/stats_meta.json" }
            ]
        }});
        let msg = BackupState::format_inspect_summary(&v);
        assert!(msg.contains("v1.0.0"));
        assert!(msg.contains("配置 1"));
        assert!(msg.contains("数据 2"));
        assert!(msg.contains("方案文件 1"));
        assert!(msg.contains("统计 2"));
    }

    #[test]
    fn sections_all_checked_means_none() {
        let st = BackupState::new();
        assert!(st.selected_sections().is_none(), "全选 → 还原全部");
        st.sec_state.set(false);
        let secs = st.selected_sections().unwrap();
        assert!(!secs.contains(&"state".to_string()));
        assert!(secs.contains(&"config".to_string()));
        // 数据域展开为五个 section 名
        for k in ["dict", "temp", "phrase", "freq", "shadow"] {
            assert!(secs.contains(&k.to_string()), "缺 {k}");
        }
    }
}
