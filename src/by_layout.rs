//! 「按候选窗排布分档」的配置值在设置端的拆 / 合。
//!
//! core 的权威实现在 `wind_config::by_layout`（`ByLayout<T>` + 标签式语法）。本模块是
//! **设置端的那一份**：`wind-config` 在本仓只是 dev-dependency（见 Cargo.toml 的注释），
//! 生产代码用不了它，故这里另写一份最小的拆 / 合。
//!
//! ⚠️ **两份实现漂移的后果是静默的**：设置页把 `"h:hide v:always"` 拆错，用户看到的是
//! 「我明明设了竖排常显，界面上却显示跟随主题」，没有报错、没有堆栈。故
//! `tests` 里用 dev-dependency 的 core 实现逐用例比对，钉住两边一致——与本仓守护
//! `capabilities.snapshot.json`、`PUNCT_DEFAULTS` 不与 core 漂移是同一套做法。

/// 档位名（与 core 的 `DIM_NAMES` 一致，含全称别名）。
const DIMS: &[(&str, bool)] = &[
    ("h", false),
    ("v", true),
    ("horizontal", false),
    ("vertical", true),
];

/// 把配置里的一个值拆成（横排档, 竖排档）。
///
/// 标量 ⇒ 两档同值；标签式 ⇒ 各取各的，没写的那档是空串（＝不覆盖）。
/// 不是标签式的字符串（含「看着像但档位名不认识」）一律当标量——与 core 同一判据，
/// 理由见那边 `DIM_NAMES` 的文档（`"x:hide"` 与 `"C:\path"` 语法同形，分辨不了，
/// 交给值域校验去报）。
pub fn split(raw: &str) -> (String, String) {
    let toks: Vec<&str> = raw.split_whitespace().collect();
    let any_dim = toks.iter().any(|t| {
        t.split_once(':')
            .is_some_and(|(k, _)| DIMS.iter().any(|(n, _)| *n == k))
    });
    if !any_dim {
        return (raw.to_string(), raw.to_string());
    }
    let (mut h, mut v) = (String::new(), String::new());
    for t in toks {
        let Some((key, val)) = t.split_once(':') else {
            continue;
        };
        match DIMS.iter().find(|(n, _)| *n == key) {
            Some((_, true)) => v = val.to_string(),
            Some((_, false)) => h = val.to_string(),
            None => {}
        }
    }
    (h, v)
}

/// 把两档合回一个配置值。
///
/// 两档相同 ⇒ 写标量（配置文件里看着就是老样子，不会平白多出 `h:`/`v:`）；
/// 不同 ⇒ 写标签式，空档省略（空＝不覆盖，写出来是噪声）。
pub fn join(h: &str, v: &str) -> String {
    if h == v {
        return h.to_string();
    }
    let mut out = String::new();
    for (name, val) in [("h", h), ("v", v)] {
        if val.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(name);
        out.push(':');
        out.push_str(val);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_applies_to_both() {
        assert_eq!(split("hide"), ("hide".into(), "hide".into()));
        assert_eq!(split(""), (String::new(), String::new()));
    }

    #[test]
    fn tagged_splits() {
        assert_eq!(split("h:hide v:always"), ("hide".into(), "always".into()));
        assert_eq!(split("v:always"), (String::new(), "always".into()));
        assert_eq!(
            split("horizontal:hide vertical:always"),
            ("hide".into(), "always".into())
        );
    }

    #[test]
    fn values_with_colons_are_not_split() {
        for raw in [r"C:\path", "18:00", "x:hide"] {
            assert_eq!(split(raw), (raw.to_string(), raw.to_string()), "{raw}");
        }
    }

    #[test]
    fn join_writes_a_scalar_when_both_sides_agree() {
        assert_eq!(join("hide", "hide"), "hide");
        assert_eq!(join("", ""), "");
    }

    #[test]
    fn join_writes_tags_when_sides_differ() {
        assert_eq!(join("hide", "always"), "h:hide v:always");
        assert_eq!(join("", "always"), "v:always", "空档省略");
        assert_eq!(join("hide", ""), "h:hide");
    }

    #[test]
    fn split_join_round_trips() {
        for raw in ["", "hide", "h:hide v:always", "v:always", "h:hide"] {
            let (h, v) = split(raw);
            assert_eq!(join(&h, &v), raw, "往返不闭合：{raw}");
        }
    }

    /// ★ 跨仓守护：本模块的拆法必须与 core 的 `ByLayout` 一致。
    ///
    /// 漂移是静默的——设置页拆错了，用户看到的是「设了没显示出来」，没有任何报错。
    /// 故这里拿 dev-dependency 里的 core 实现逐用例比对。
    #[test]
    fn split_matches_core_by_layout() {
        use wind_config::by_layout::ByLayout;

        #[derive(serde::Deserialize)]
        struct Holder {
            x: ByLayout<String>,
        }

        for raw in [
            "",
            "hide",
            "always",
            "h:hide v:always",
            "v:always",
            "h:hide",
            "horizontal:hide vertical:always",
            r"C:\path",
            "18:00",
            "x:hide",
        ] {
            let doc = format!("x = {}", toml::Value::String(raw.to_string()));
            let core: Holder = toml::from_str(&doc).unwrap_or_else(|e| panic!("{raw}: {e}"));
            let mine = split(raw);
            assert_eq!(
                (
                    core.x.as_str(false).to_string(),
                    core.x.as_str(true).to_string()
                ),
                mine,
                "与 core 的拆法不一致：{raw}"
            );
        }
    }
}
