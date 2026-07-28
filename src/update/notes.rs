//! 发布说明清洗。
//!
//! 发布说明同时服务于 GitHub Release 页面和应用内更新对话框，前者容忍自动生成的
//! 段落（Full Changelog、What's Changed…），后者不该让用户看到这些。约定用
//! `<!-- user-facing:start -->` / `<!-- user-facing:end -->` 圈出面向用户的部分。
//!
//! 三级回退，逐级放宽，都取不到就返回空串 —— 发布说明缺失不阻断升级。

/// 面向用户段落的标记。
const MARK_START: &str = "<!-- user-facing:start -->";
const MARK_END: &str = "<!-- user-facing:end -->";

/// 模板里的占位符：写了标记但没填内容，视同没写。
const PLACEHOLDER: &str = "暂未填写";

/// GitHub 自动生成、对用户无意义的段落标题。
const NOISE_HEADINGS: &[&str] = &[
    "**Full Changelog**",
    "## What's Changed",
    "## New Contributors",
];

/// 从原始发布说明中提取面向用户的部分，并转成纯文本。
///
/// 末尾的纯文本化是必要的：唯一的消费者是 windui 的纯文本 label，没有 markdown 渲染，
/// 不处理的话 `### 新增` 会把井号原样显示给用户。
pub fn clean(raw: &str) -> String {
    let extracted = extract_marked(raw)
        .or_else(|| extract_changelog_section(raw))
        .unwrap_or_else(|| fallback_strip_noise(raw));
    to_plain_text(&extracted)
}

/// markdown → 纯文本：去标题井号、统一列表符号、剥粗体/斜体/行内代码标记。
fn to_plain_text(s: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in s.lines() {
        let t = line.trim_end();
        // 标题：去掉行首的 #（含其后空格）
        let t = t.trim_start_matches('#');
        let t = t.strip_prefix(' ').unwrap_or(t);
        // 列表：* / + 统一成 -，保持缩进不变
        let t = if let Some(rest) = t.strip_prefix("* ") {
            format!("- {rest}")
        } else if let Some(rest) = t.strip_prefix("+ ") {
            format!("- {rest}")
        } else {
            t.to_string()
        };
        out.push(strip_inline_marks(&t));
    }
    out.join("\n").trim().to_string()
}

/// 剥除成对的行内标记：`**粗**` `*斜*` `` `码` ``。
///
/// 逐字符扫描而非全局替换 —— 全局删 `*` 会把「3 * 5」这类正文里的星号也吃掉。
fn strip_inline_marks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' || c == '`' {
            // 找到配对的闭合符号，则**开闭一起去掉**、中间内容原样保留。
            // 只判断"后面还有没有同种符号"是不够的：那会让 `**粗**` 的最后一个
            // 星号找不到配对而被留下，得到 `粗*`。
            if let Some(rel) = chars[i + 1..].iter().position(|&x| x == c) {
                let close = i + 1 + rel;
                out.extend(&chars[i + 1..close]);
                i = close + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// ① 标记区间。内容为占位符时视作未填写，交给下一级。
fn extract_marked(raw: &str) -> Option<String> {
    let start = raw.find(MARK_START)? + MARK_START.len();
    let end = raw[start..].find(MARK_END)? + start;
    let body = raw[start..end].trim();
    if body.is_empty() || body == PLACEHOLDER {
        return None;
    }
    Some(body.to_string())
}

/// ② `## 更新记录` 标题到下一个分隔线（或文末）。
fn extract_changelog_section(raw: &str) -> Option<String> {
    let head = raw.find("## 更新记录")?;
    let rest = &raw[head + "## 更新记录".len()..];
    let body = match rest.find("\n---") {
        Some(i) => &rest[..i],
        None => rest,
    };
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    Some(body.to_string())
}

/// ③ 兜底：逐行剔除自动生成的噪声段落。
///
/// 命中噪声标题后跳过其后所有内容，直到遇到下一个 `##` 标题为止 —— 噪声标题下的
/// 条目列表同样不该展示。
fn fallback_strip_noise(raw: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in raw.lines() {
        let t = line.trim();
        if NOISE_HEADINGS.iter().any(|h| t.starts_with(h)) {
            skipping = true;
            continue;
        }
        // 新的二级标题结束跳过区（噪声标题本身已在上一分支被吃掉）
        if skipping {
            if t.starts_with("## ") {
                skipping = false;
            } else {
                continue;
            }
        }
        out.push(line);
    }
    out.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marked_section_wins() {
        let raw = "\
## What's Changed
* 一堆自动生成的东西

<!-- user-facing:start -->
### 新增
- 在线升级
<!-- user-facing:end -->

**Full Changelog**: https://example.com/compare";
        assert_eq!(clean(raw), "新增\n- 在线升级");
    }

    #[test]
    fn placeholder_marker_falls_through_to_next_level() {
        let raw = "\
<!-- user-facing:start -->
暂未填写
<!-- user-facing:end -->

## 更新记录
- 修了个 bug
---
其它";
        assert_eq!(clean(raw), "- 修了个 bug");
    }

    #[test]
    fn empty_marker_falls_through() {
        let raw = "<!-- user-facing:start -->\n\n<!-- user-facing:end -->\n\n## 更新记录\n- 改进";
        assert_eq!(clean(raw), "- 改进");
    }

    #[test]
    fn unclosed_marker_falls_through_instead_of_panicking() {
        let raw = "<!-- user-facing:start -->\n### 新增\n- 忘了闭合\n\n## 更新记录\n- 兜底内容";
        assert_eq!(clean(raw), "- 兜底内容");
    }

    #[test]
    fn changelog_section_stops_at_divider() {
        let raw = "## 更新记录\n- 甲\n- 乙\n\n---\n\n## 安装说明\n别显示我";
        assert_eq!(clean(raw), "- 甲\n- 乙");
    }

    #[test]
    fn changelog_section_without_divider_takes_rest() {
        let raw = "## 更新记录\n- 只有这些";
        assert_eq!(clean(raw), "- 只有这些");
    }

    #[test]
    fn fallback_strips_noise_headings_and_their_items() {
        let raw = "\
### 亮点
- 值得看的

## What's Changed
* by @someone in #1
* by @other in #2

## New Contributors
* @newbie made their first contribution

**Full Changelog**: https://example.com/compare";
        assert_eq!(clean(raw), "亮点\n- 值得看的");
    }

    #[test]
    fn fallback_resumes_at_next_heading_after_noise() {
        let raw = "\
## What's Changed
* 噪声条目

## 说明
- 这段要保留";
        assert_eq!(clean(raw), "说明\n- 这段要保留");
    }

    #[test]
    fn plain_text_strips_heading_hashes() {
        assert_eq!(
            to_plain_text("# 一级\n## 二级\n### 三级"),
            "一级\n二级\n三级"
        );
    }

    #[test]
    fn plain_text_normalizes_list_markers() {
        assert_eq!(to_plain_text("* 甲\n+ 乙\n- 丙"), "- 甲\n- 乙\n- 丙");
    }

    #[test]
    fn plain_text_strips_paired_inline_marks() {
        assert_eq!(to_plain_text("**重要**修复"), "重要修复");
        assert_eq!(to_plain_text("使用 `--flag` 开启"), "使用 --flag 开启");
    }

    /// 全局删 `*` 会把正文里的乘号也吃掉，故只在能找到配对符号时才当标记。
    #[test]
    fn plain_text_keeps_unpaired_asterisk() {
        assert_eq!(to_plain_text("宽度 3 * 5 单位"), "宽度 3 * 5 单位");
    }

    /// 井号后无空格（如 `#123` 引用编号）不该被当成标题吃掉数字。
    #[test]
    fn plain_text_handles_hash_without_space() {
        assert_eq!(to_plain_text("### 修复"), "修复");
    }

    #[test]
    fn empty_input_yields_empty() {
        assert_eq!(clean(""), "");
        assert_eq!(clean("   \n  \n"), "");
    }

    /// 发布说明拉取失败或为空不应阻断升级，clean 必须对任意输入都返回而不 panic。
    #[test]
    fn arbitrary_input_does_not_panic() {
        for raw in [
            "<!-- user-facing:end --> 只有结束标记",
            "## 更新记录",
            "---\n---\n---",
            "**Full Changelog**",
        ] {
            let _ = clean(raw);
        }
    }
}
