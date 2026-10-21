//! 词条权重的输入校验与说明文案（加词小窗与词库管理对话框、纯词列表导入共用）。

/// 权重的取值范围：core 按 `i32` 存（`dict.add` 取参后 `as i32`，用户词文本与日志两头都是
/// `i32`）。越界的数发过去会被**静默截断**成另一个数，故在设置端拦。
pub const WEIGHT_MIN: i32 = i32::MIN;
pub const WEIGHT_MAX: i32 = i32::MAX;

/// 权重输入框旁的说明：给个参照值比写出 i32 两端更有用；范围只在越界时报。
pub fn weight_hint(default: i32) -> String {
    format!("整数，默认 {default}，越大越靠前")
}

/// 解析权重输入。空串与非整数报「需为整数」，越界报范围。
pub fn parse_weight(s: &str) -> Result<i32, String> {
    use std::num::IntErrorKind;
    match s.trim().parse::<i32>() {
        Ok(w) => Ok(w),
        Err(e) => match e.kind() {
            IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
                Err(format!("权重超出范围（{WEIGHT_MIN} ~ {WEIGHT_MAX}）"))
            }
            _ => Err("权重需为整数".to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 范围内（含两端、负数、前后空白）照收。
    #[test]
    fn weight_within_range_is_accepted() {
        assert_eq!(parse_weight("1200"), Ok(1200));
        assert_eq!(parse_weight(" -5 "), Ok(-5));
        assert_eq!(parse_weight("0"), Ok(0));
        assert_eq!(parse_weight("2147483647"), Ok(i32::MAX));
        assert_eq!(parse_weight("-2147483648"), Ok(i32::MIN));
    }

    /// 越界报范围而不是静默截断（core 侧 `as i32` 会把它变成另一个数）。
    #[test]
    fn weight_out_of_range_is_rejected_with_range() {
        for s in ["2147483648", "-2147483649", "99999999999"] {
            let e = parse_weight(s).unwrap_err();
            assert!(e.contains("超出范围"), "{s:?} 应报越界，实得 {e:?}");
            assert!(e.contains("2147483647"), "报错要带上限：{e:?}");
        }
    }

    /// 空串、小数、非数字报「需为整数」，不静默按默认值提交。
    #[test]
    fn weight_non_integer_is_rejected() {
        for s in ["", "  ", "1.5", "abc", "1e3"] {
            assert_eq!(parse_weight(s), Err("权重需为整数".to_string()), "{s:?}");
        }
    }

    /// 说明里的参照值就是传入的默认值，且本身能通过校验。
    #[test]
    fn weight_hint_names_the_default() {
        for d in [0, 1200, 1800] {
            assert!(weight_hint(d).contains(&format!("默认 {d}，")), "{d}");
            assert_eq!(parse_weight(&d.to_string()), Ok(d));
        }
        assert!(parse_weight(&WEIGHT_MIN.to_string()).is_ok());
        assert!(parse_weight(&WEIGHT_MAX.to_string()).is_ok());
    }
}
