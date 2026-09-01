//! 纯词列表（一行一个词、无编码）的识别与解析 —— 纯函数，不碰 RPC 与信号。
//!
//! 编码由目标方案现场生成（`dict.encodeWords`，分批），出好码的行拼成 TSV 走既有导入
//! 通道。设计见 `docs/wordlist-import-design.md`。
//!
//! **识别为什么要严格**：现有格式探测把「不含制表符、无 `wind_dict:`、无 `...`」的文本
//! 判为 Unknown 并明确报错。纯词列表恰好落在这一支——收编它等于放弃那句报错，用户误选
//! `.md` / 日志 / 代码文件时就从「无法识别的词库格式」退化成一屏垃圾预览。故设两道关：
//! 先整体 sanity check（[`looks_like_word_list`]），过了再逐行过滤。

/// 单个词条的字数上限。超出的多半是整句/整段，不是词。
const MAX_WORD_CHARS: usize = 12;

/// 判定为纯词列表所需的「像词行」占比下限。
///
/// 留 10% 余量而非要求全中：真实词表常夹带零星标题行、分隔线、编号残留，
/// 一刀切会把本来能用的文件挡在门外——那些行由逐行过滤计入 skipped 即可。
const MIN_WORD_RATIO: f64 = 0.9;

/// 每批出码的词数。批越大 RPC 往返越少，但单批必须稳在 `rpc::CALL_TIMEOUT`（5s）内。
pub const ENCODE_BATCH: usize = 1000;

/// 纯词列表解析结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WordList {
    /// 去重后的词（保留首次出现的顺序）。
    pub words: Vec<String>,
    /// 不像词条而被丢弃的行数（不含空行与注释行——那些是格式的一部分，不算"跳过"）。
    pub skipped: usize,
    /// 因重复而合并掉的行数。
    pub duplicates: usize,
}

/// 剥 UTF-8 BOM（记事本另存为 UTF-8 会带上，不剥的话首行首字符判定必错）。
fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// 这一行是否为格式噪声（空行 / `#` 注释），既不算词也不算跳过。
fn is_ignorable(line: &str) -> bool {
    line.is_empty() || line.starts_with('#')
}

/// 是否为 CJK 表意文字（含扩展区与兼容区）。
///
/// 刻意不用「非 ASCII 即中文」：那会把中文标点、全角符号、假名、谚文一并放行，
/// 而它们出现在行里恰恰说明这行不是词条。
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF      // 扩展 A
        | 0x4E00..=0x9FFF    // 基本区
        | 0xF900..=0xFAFF    // 兼容表意
        | 0x20000..=0x2A6DF  // 扩展 B
        | 0x2A700..=0x2EBEF  // 扩展 C–F
        | 0x2F800..=0x2FA1F  // 兼容补充
        | 0x30000..=0x3134F  // 扩展 G–H
    )
}

/// 这一行看着像不像一个词条：全 CJK、长度在上限内、无内部空白。
///
/// 无内部空白是有意为之：`你好 世界` 究竟是一个词条还是两个，无法从文件本身判定，
/// 与其猜错不如跳过并报数，让用户先把文件整理好。
pub fn looks_like_word(line: &str) -> bool {
    let n = line.chars().count();
    n > 0 && n <= MAX_WORD_CHARS && line.chars().all(is_cjk)
}

/// 整体 sanity check：这份文本该不该被当作纯词列表。
///
/// 只看比例，不看绝对数量——三行的小词表和三万行的大词表同样合法。
pub fn looks_like_word_list(text: &str) -> bool {
    let mut total = 0usize;
    let mut like = 0usize;
    for line in strip_bom(text).lines() {
        let t = line.trim();
        if is_ignorable(t) {
            continue;
        }
        total += 1;
        if looks_like_word(t) {
            like += 1;
        }
    }
    total > 0 && (like as f64) >= (total as f64) * MIN_WORD_RATIO
}

/// 解析纯词列表。调用方应先用 [`looks_like_word_list`] 过第一道关
/// （或由用户在格式下拉里强制指定），本函数只负责逐行过滤与去重。
pub fn parse_word_list(text: &str) -> WordList {
    let mut out = WordList::default();
    let mut seen = std::collections::HashSet::new();
    for line in strip_bom(text).lines() {
        let t = line.trim();
        if is_ignorable(t) {
            continue;
        }
        if !looks_like_word(t) {
            out.skipped += 1;
            continue;
        }
        if seen.insert(t.to_string()) {
            out.words.push(t.to_string());
        } else {
            out.duplicates += 1;
        }
    }
    out
}

/// 出好码的行 → TSV 文本（`code<TAB>text<TAB>weight`），交给既有的 TSV 导入通道。
///
/// 空码行**必须由调用方先剔除**：TSV 解析器会把空 code 的行判为非法并计进它自己的
/// skipped，那样统计口径就分裂成两处，界面上报的数字对不上。
pub fn to_tsv(rows: &[(String, String)], weight: i64) -> String {
    let mut s = String::new();
    for (code, text) in rows {
        s.push_str(code);
        s.push('\t');
        s.push_str(text);
        s.push('\t');
        s.push_str(&weight.to_string());
        s.push('\n');
    }
    s
}

/// 按 [`ENCODE_BATCH`] 切批。
pub fn batches(words: &[String]) -> impl Iterator<Item = &[String]> {
    words.chunks(ENCODE_BATCH)
}

/// 单批导入 TSV 的字节上限。
///
/// 这条路的 TSV 是内存里现拼的，没有文件可指，只能走 `content`——于是仍受 RPC 读帧
/// 上限（`rpc::MAX_FRAME`，16MB）约束。词表规模由用户决定、我们无从预知，故按字节切批。
///
/// 取 4MB 而不是贴着 16MB：JSON 字符串会把每行的 2 个 `\t` 与 1 个 `\n` 各转义成两
/// 字节，行长约 15 字节时放大约 20%；再算上请求骨架与 core 侧同样 16MB 的读帧上限，
/// 留三倍余量。代价只是多几次 RPC——19 万词的 TSV 约 3MB，根本切不出第二批。
pub const IMPORT_BATCH_BYTES: usize = 4 * 1024 * 1024;

/// 按 TSV 字节数把出好码的行切批（导入用）。
///
/// ★ 判据是**字节**而不是条数：撞的是字节上限，而行长随编码方案与词长变化很大，
/// 按条数切等于用一个跟约束无关的量去逼近它。
///
/// ⚠️ **每批至少一行**：单行就超上限时也照发（让 core 去报错），否则切批会原地死循环。
pub fn import_batches(
    rows: &[(String, String)],
    weight: i64,
    max_bytes: usize,
) -> Vec<&[(String, String)]> {
    let tail = weight.to_string().len() + 3; // 2 个 \t + 1 个 \n
    let mut out = Vec::new();
    let (mut start, mut bytes) = (0usize, 0usize);
    for (i, (code, text)) in rows.iter().enumerate() {
        let line = code.len() + text.len() + tail;
        // i > start 保证批非空：单行超限时不在这里切，留给下一轮或收尾。
        if bytes + line > max_bytes && i > start {
            out.push(&rows[start..i]);
            start = i;
            bytes = 0;
        }
        bytes += line;
    }
    if start < rows.len() {
        out.push(&rows[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_plain_word_list() {
        let text = "中国\n计算机\n人工智能\n";
        assert!(looks_like_word_list(text));
        let p = parse_word_list(text);
        assert_eq!(p.words, vec!["中国", "计算机", "人工智能"]);
        assert_eq!(p.skipped, 0);
    }

    #[test]
    fn ignores_blank_and_comment_lines() {
        let text = "# 我的词表\n\n中国\n\n  计算机  \n";
        assert!(looks_like_word_list(text));
        let p = parse_word_list(text);
        assert_eq!(p.words, vec!["中国", "计算机"], "两侧空白应剥净");
        assert_eq!(p.skipped, 0, "空行与注释是格式的一部分，不算跳过");
    }

    #[test]
    fn strips_bom() {
        let p = parse_word_list("\u{feff}中国\n");
        assert_eq!(p.words, vec!["中国"], "BOM 不剥会让首行判定必错");
    }

    #[test]
    fn dedupes_keeping_first_occurrence() {
        let p = parse_word_list("中国\n计算机\n中国\n");
        assert_eq!(p.words, vec!["中国", "计算机"]);
        assert_eq!(p.duplicates, 1);
    }

    /// 误选非词表文件时必须仍然判否——收编 Unknown 分支的代价就在这里，
    /// 判据一松，用户选错文件就从明确报错退化成一屏垃圾预览。
    #[test]
    fn rejects_files_that_are_not_word_lists() {
        for (name, text) in [
            (
                "markdown",
                "# 标题\n\n这是一段正文，讲了一些事情。\n\n- 列表项\n- 另一项\n",
            ),
            ("代码", "fn main() {\n    println!(\"hello\");\n}\n"),
            (
                "日志",
                "2026-08-10 12:00:01 INFO 启动完成\n2026-08-10 12:00:02 WARN 超时\n",
            ),
            ("空文件", "\n\n\n"),
            ("全注释", "# a\n# b\n"),
        ] {
            assert!(!looks_like_word_list(text), "{name} 不该被当作纯词列表");
        }
    }

    #[test]
    fn rejects_lines_that_are_not_words() {
        // 含空格、含 ASCII、含中文标点、过长——都不是词条。
        for line in [
            "你好 世界",
            "中国 China",
            "中国。",
            "ＡＢＣ",
            "这是一个特别特别长的句子不像是一个词条",
        ] {
            assert!(!looks_like_word(line), "「{line}」不该判为词条");
        }
        assert!(looks_like_word("中国"));
        assert!(
            looks_like_word("𰻞𰻞面"),
            "扩展区生僻字仍是词条（能不能出码是另一回事）"
        );
    }

    /// 零星噪声行不该整份否掉——真实词表常夹带标题、分隔线。
    #[test]
    fn tolerates_a_few_noise_lines() {
        let mut text = String::from("=== 常用词 ===\n");
        for i in 0..30 {
            text.push_str(&format!(
                "词条{}\n",
                "一二三四五六七八九十".chars().nth(i % 10).unwrap()
            ));
        }
        assert!(
            looks_like_word_list(&text),
            "一行噪声 + 30 行词，应通过整体判据"
        );
        let p = parse_word_list(&text);
        assert_eq!(p.skipped, 1, "噪声行由逐行过滤计入 skipped");
    }

    #[test]
    fn tsv_carries_code_text_and_weight() {
        let rows = vec![
            ("khgo".to_string(), "中国".to_string()),
            ("ni hao".to_string(), "你好".to_string()),
        ];
        let tsv = to_tsv(&rows, 1200);
        assert_eq!(tsv, "khgo\t中国\t1200\nni hao\t你好\t1200\n");
    }

    #[test]
    fn batches_split_at_the_limit() {
        let words: Vec<String> = (0..ENCODE_BATCH * 2 + 7).map(|i| i.to_string()).collect();
        let sizes: Vec<usize> = batches(&words).map(|b| b.len()).collect();
        assert_eq!(sizes, vec![ENCODE_BATCH, ENCODE_BATCH, 7]);
    }
}
