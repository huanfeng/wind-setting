//! 外观页「跟随主题（值）」（R8.1 / R8.2，整改项 F4 / F5）。
//!
//! core 的 `theme.resolved` 给出当前主题对外观可覆盖键解析后的值；本模块把它折成
//! 界面文案与勾选行的读写规则。

use serde_json::Value;

/// `theme.resolved` 的回包（core 侧 `ThemeFollowValues`，camelCase JSON）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThemeFollow {
    pub font_size: i32,
    pub font_family: Option<String>,
    pub font_weight: i32,
    pub pager_bar_display: String,
    pub page_number_display: String,
    pub langbar_text: [Option<String>; 4],
}

impl ThemeFollow {
    /// 解析 `theme.resolved` 的回包；`null`（两级主题都加载失败）或形状不对时为 `None`。
    pub fn from_value(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let int = |k: &str| obj.get(k).and_then(Value::as_i64).unwrap_or(0) as i32;
        let s = |k: &str| {
            obj.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let mut langbar_text: [Option<String>; 4] = Default::default();
        if let Some(arr) = obj.get("langbarText").and_then(Value::as_array) {
            for (slot, c) in langbar_text.iter_mut().zip(arr) {
                *slot = c.as_str().map(str::to_string);
            }
        }
        Some(Self {
            font_size: int("fontSize"),
            font_family: obj
                .get("fontFamily")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            font_weight: int("fontWeight"),
            pager_bar_display: s("pagerBarDisplay"),
            page_number_display: s("pageNumberDisplay"),
            langbar_text,
        })
    }
}

/// 来源列 / 无主题数据时的通用文案（R8.3 三个词之一）。
pub const FOLLOW_THEME: &str = "跟随主题";

/// 拿不到 `theme.resolved` 时勾选的初值：R3 出厂值（core `Config::default` 渲染时的实际
/// 取值），与主题无关。字族给空串，由字体下拉按「不在列表 ⇒ 首项」落到首个可用字体。
const FACTORY_FONT_SIZE: i64 = 18;
const FACTORY_FONT_WEIGHT: i64 = 400;

/// 该键「跟随」时落盘的 R3 哨兵：字号 `0`、字族 `""`、字重 `0`。
pub fn sentinel_of(key: &str) -> Value {
    match key {
        "ui.font.family" => Value::String(String::new()),
        _ => Value::from(0),
    }
}

/// 配置值是否表示「跟随」。数值按**数值**比：配置里的 `font_size` 是浮点 `0.0`，
/// 与 `sentinel_of` 的整数 `0` 在 `Value` 层不相等。缺键 = 跟随（出厂值就是哨兵）。
pub fn is_sentinel(key: &str, v: Option<&Value>) -> bool {
    let Some(v) = v else {
        return true;
    };
    match sentinel_of(key) {
        Value::String(s) => v.as_str().is_none_or(|x| x == s),
        _ => v.as_f64().is_none_or(|x| x == 0.0),
    }
}

/// 勾选瞬间控件的初值 = 主题当前值。**不能**是哨兵，否则勾上等于没勾（Review Focus 5）。
pub fn seed_on_check(key: &str, tf: &ThemeFollow) -> Value {
    match key {
        "ui.candidate.font_size" if tf.font_size > 0 => Value::from(tf.font_size),
        "ui.font.family" => Value::String(tf.font_family.clone().unwrap_or_default()),
        "ui.font.weight" if tf.font_weight > 0 => Value::from(tf.font_weight),
        _ => seed_opt(key, None),
    }
}

/// [`seed_on_check`] 的可空版本：主题数据缺席时回落 R3 出厂值。
pub fn seed_opt(key: &str, tf: Option<&ThemeFollow>) -> Value {
    if let Some(tf) = tf {
        return seed_on_check(key, tf);
    }
    match key {
        "ui.candidate.font_size" => Value::from(FACTORY_FONT_SIZE),
        "ui.font.weight" => Value::from(FACTORY_FONT_WEIGHT),
        _ => Value::String(String::new()),
    }
}

/// 未勾选时控件位显示的跟随值：`18` / `系统字体` / `常规 400`。
pub fn theme_follow_text(key: &str, tf: &ThemeFollow) -> String {
    match key {
        "ui.candidate.font_size" => tf.font_size.to_string(),
        "ui.font.family" => tf
            .font_family
            .clone()
            .unwrap_or_else(|| "系统字体".to_string()),
        "ui.font.weight" if tf.font_weight <= 0 => "主题未指定".to_string(),
        "ui.font.weight" => {
            let v = tf.font_weight.to_string();
            option_label(key, &v).unwrap_or(v)
        }
        _ => String::new(),
    }
}

/// [`theme_follow_text`] 的可空版本：主题数据缺席时只说「跟随主题」，不编一个值出来。
pub fn theme_follow_text_opt(key: &str, tf: Option<&ThemeFollow>) -> String {
    match tf {
        Some(tf) => theme_follow_text(key, tf),
        None => FOLLOW_THEME.to_string(),
    }
}

/// 翻页栏下拉空值档的文案：`跟随主题（隐藏）`。档位名取清单里同一键的 options。
pub fn pager_follow_label(tf: &ThemeFollow) -> String {
    follow_label("ui.candidate.pager_bar_display", &tf.pager_bar_display)
}

/// 页码下拉空值档的文案：`跟随主题（显示）`。
pub fn page_number_follow_label(tf: &ThemeFollow) -> String {
    follow_label("ui.candidate.page_number_display", &tf.page_number_display)
}

/// 下拉空值档（R8.2「跟随」档）的文案；只认得本模块管的两个键，其余返回 `None`。
pub fn empty_option_label(key: &str, tf: Option<&ThemeFollow>) -> Option<String> {
    let f = match key {
        "ui.candidate.pager_bar_display" => pager_follow_label,
        "ui.candidate.page_number_display" => page_number_follow_label,
        _ => return None,
    };
    Some(tf.map(f).unwrap_or_else(|| FOLLOW_THEME.to_string()))
}

fn follow_label(key: &str, value: &str) -> String {
    let name = option_label(key, value).unwrap_or_else(|| value.to_string());
    format!("{FOLLOW_THEME}（{name}）")
}

/// 清单里 `key` 的 options 中 `value` 那一档的 label。与控件用的是**同一份**清单，
/// 不另写映射——档位名改了两边一起变。
fn option_label(key: &str, value: &str) -> Option<String> {
    static EMBEDDED: std::sync::OnceLock<crate::manifest::Manifest> = std::sync::OnceLock::new();
    EMBEDDED
        .get_or_init(crate::manifest::Manifest::from_embedded)
        .get(key)?
        .options
        .iter()
        .find(|o| o.value == value)
        .map(|o| o.label.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mock_tf() -> ThemeFollow {
        ThemeFollow {
            font_size: 18,
            font_family: None,
            font_weight: 0,
            pager_bar_display: "auto".into(),
            page_number_display: "show".into(),
            langbar_text: [None, None, None, None],
        }
    }

    #[test]
    fn follow_text_for_font_size_is_theme_value() {
        let tf = ThemeFollow {
            font_size: 18,
            ..mock_tf()
        };
        assert_eq!(theme_follow_text("ui.candidate.font_size", &tf), "18");
    }

    /// Review Focus 5：勾选瞬间写入主题当前值，不能写 0，否则会悄悄又变回跟随。
    #[test]
    fn checking_font_size_seeds_theme_value_not_zero() {
        let tf = ThemeFollow {
            font_size: 18,
            ..mock_tf()
        };
        assert_eq!(seed_on_check("ui.candidate.font_size", &tf), json!(18));
    }

    #[test]
    fn unchecking_writes_r3_sentinel() {
        assert_eq!(sentinel_of("ui.candidate.font_size"), json!(0));
        assert_eq!(sentinel_of("ui.font.family"), json!(""));
    }

    #[test]
    fn pager_empty_option_label_shows_theme_choice() {
        let tf = ThemeFollow {
            pager_bar_display: "hide".into(),
            ..mock_tf()
        };
        assert_eq!(pager_follow_label(&tf), "跟随主题（隐藏）");
    }

    #[test]
    fn theme_follow_missing_shows_plain_follow() {
        // theme.resolved 返回 null（两级主题都坏）时不崩，文案退化为「跟随主题」
        assert_eq!(
            theme_follow_text_opt("ui.candidate.font_size", None),
            "跟随主题"
        );
    }

    /// 字族：主题未指定（null）= 系统字体；指定了就原样给名字。
    #[test]
    fn follow_text_for_font_family() {
        assert_eq!(theme_follow_text("ui.font.family", &mock_tf()), "系统字体");
        let tf = ThemeFollow {
            font_family: Some("思源黑体".into()),
            ..mock_tf()
        };
        assert_eq!(theme_follow_text("ui.font.family", &tf), "思源黑体");
    }

    /// 字重：取清单里同一键的档位名；0 = 主题未指定；清单没列的数值原样给。
    #[test]
    fn follow_text_for_font_weight() {
        let tf = ThemeFollow {
            font_weight: 400,
            ..mock_tf()
        };
        assert_eq!(theme_follow_text("ui.font.weight", &tf), "常规 400");
        assert_eq!(
            theme_follow_text("ui.font.weight", &mock_tf()),
            "主题未指定"
        );
        let odd = ThemeFollow {
            font_weight: 450,
            ..mock_tf()
        };
        assert_eq!(theme_follow_text("ui.font.weight", &odd), "450");
    }

    /// 字重 / 字族勾选时的初值：主题未指定就给 R3 出厂值（400 / 空串 → 下拉首项）。
    #[test]
    fn seeds_fall_back_to_factory_when_theme_unspecified() {
        assert_eq!(seed_on_check("ui.font.weight", &mock_tf()), json!(400));
        let tf = ThemeFollow {
            font_weight: 700,
            ..mock_tf()
        };
        assert_eq!(seed_on_check("ui.font.weight", &tf), json!(700));
        assert_eq!(seed_on_check("ui.font.family", &mock_tf()), json!(""));
        assert_eq!(seed_opt("ui.candidate.font_size", None), json!(18));
        assert_eq!(seed_opt("ui.font.weight", None), json!(400));
    }

    #[test]
    fn page_number_empty_option_label_shows_theme_choice() {
        assert_eq!(page_number_follow_label(&mock_tf()), "跟随主题（显示）");
        let tf = ThemeFollow {
            pager_bar_display: "auto".into(),
            ..mock_tf()
        };
        assert_eq!(pager_follow_label(&tf), "跟随主题（大于一页时显示）");
    }

    /// 是否处于跟随：数值按数值比（配置里 font_size 是浮点 0.0），缺键 = 跟随。
    #[test]
    fn is_sentinel_compares_numerically_and_treats_missing_as_follow() {
        assert!(is_sentinel("ui.candidate.font_size", Some(&json!(0.0))));
        assert!(is_sentinel("ui.candidate.font_size", None));
        assert!(!is_sentinel("ui.candidate.font_size", Some(&json!(16.0))));
        assert!(is_sentinel("ui.font.family", Some(&json!(""))));
        assert!(!is_sentinel("ui.font.family", Some(&json!("等线"))));
        assert!(is_sentinel("ui.font.weight", Some(&json!(0))));
    }

    #[test]
    fn parses_camel_case_rpc_reply_and_null() {
        let v = json!({
            "themeId": "default", "fontSize": 18, "fontFamily": null, "fontWeight": 0,
            "pagerBarDisplay": "auto", "pageNumberDisplay": "show",
            "langbarText": ["#000000", null, "#000000", "#FFFFFF"],
        });
        let tf = ThemeFollow::from_value(&v).expect("应能解析");
        assert_eq!(tf.font_size, 18);
        assert_eq!(tf.font_family, None);
        assert_eq!(tf.pager_bar_display, "auto");
        assert_eq!(tf.langbar_text[1], None);
        assert_eq!(tf.langbar_text[3].as_deref(), Some("#FFFFFF"));
        assert_eq!(ThemeFollow::from_value(&Value::Null), None);
    }
}
