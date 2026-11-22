//! 离线 mock（`WIND_RPC_MOCK=1`）下 `softkeyboard.*` 的回复：两张出厂面 + 一张用户面。
//!
//! 只为截图与界面测试服务，不复刻 core 的写入语义——`softkeyboard.apply` 原样回同一份视图。
//! 要断言「点了之后发出去什么」请看 `rpc::take_mock_calls`，不要看 mock 的状态。

use serde_json::{Value, json};

use super::model::all_slot_names;

/// 按画布四行铺键位：每行用空格分隔，`.` = 空。
fn keys(rows: [&str; 4], shift: [&str; 4], defaults: Option<([&str; 4], [&str; 4])>) -> Value {
    let flat = |rows: [&str; 4]| -> Vec<Option<String>> {
        let mut out = Vec::new();
        for (r, line) in rows.iter().enumerate() {
            let n = super::model::SLOT_ROWS[r].len();
            let toks: Vec<&str> = line.split(' ').filter(|s| !s.is_empty()).collect();
            for i in 0..n {
                out.push(toks.get(i).filter(|t| **t != ".").map(|t| match *t {
                    "\\." => ".".to_string(),
                    t => t.to_string(),
                }));
            }
        }
        out
    };
    let (b, s) = (flat(rows), flat(shift));
    let (db, ds) = match defaults {
        Some((d, dsh)) => (flat(d), flat(dsh)),
        None => (b.clone(), s.clone()),
    };
    Value::Array(
        all_slot_names()
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                json!({
                    "slot": slot,
                    "base": b[i], "shift": s[i],
                    "defaultBase": db[i], "defaultShift": ds[i],
                    "basePatched": b[i] != db[i], "shiftPatched": s[i] != ds[i],
                })
            })
            .collect(),
    )
}

const PUNCT: [&str; 4] = [
    "· ！ ？ 。 ， ； … — ‧ （ ） 〜 ＝",
    "“ ” ‘ ’ 《 》 【 】 ｛ ｝ 〔 〕 ＼",
    "、 。 ， ； ： ？ ！ ‥ ※ ： ＇",
    "〃 〆 ‰ § † ‖ 〓 ＜ ＞ ／",
];
const PUNCT_SHIFT: [&str; 4] = [
    "～ ¡ ¿ ． 、 ： ‥ － · 〔 〕 ‐ ≡",
    "「 」 『 』 〈 〉 〖 〗 ｟ ｠ ⁅ ⁆ ﹨",
    "﹑ ｡ ﹐ ﹔ ﹕ ﹖ ﹗ ⋯ ＊ ﹕ ＂",
    "〝 〟 ％ ¶ ‡ ∥ ▬ ≪ ≫ ÷",
];

pub fn view() -> Value {
    // 标点面改过一个键（q），演示「已修改」角标。
    let mut punct_now = PUNCT;
    punct_now[1] = "√ ” ‘ ’ 《 》 【 】 ｛ ｝ 〔 〕 ＼";
    json!({ "pages": [
        {
            "id": "pc", "name": "PC键盘", "defaultName": "PC键盘",
            "userCreated": false, "sendKeys": true, "hasUserEntry": false,
            "keys": keys(
                ["` 1 2 3 4 5 6 7 8 9 0 - =", "q w e r t y u i o p [ ] \\", "a s d f g h j k l ; '", "z x c v b n m , \\. /"],
                ["~ ! @ # $ % ^ & * ( ) _ +", "Q W E R T Y U I O P { } |", "A S D F G H J K L : \"", "Z X C V B N M < > ?"],
                None,
            ),
        },
        {
            "id": "punct", "name": "标点符号", "defaultName": "标点符号",
            "userCreated": false, "sendKeys": false, "hasUserEntry": true,
            "keys": keys(punct_now, PUNCT_SHIFT, Some((PUNCT, PUNCT_SHIFT))),
        },
        {
            "id": "mine", "name": "我的符号", "defaultName": "我的符号",
            "userCreated": true, "sendKeys": false, "hasUserEntry": true,
            "keys": keys(
                ["☆ ★ ○ ● ◎ ◇ ◆ □ ■ △ ▲ . .", ". . . . . . . . . . . . .", ". . . . . . . . . . .", ". . . . . . . . . ."],
                [". . . . . . . . . . . . .", ". . . . . . . . . . . . .", ". . . . . . . . . . .", ". . . . . . . . . ."],
                None,
            ),
        },
    ]})
}

pub fn reply(method: &str, _params: &Value) -> Value {
    match method {
        "softkeyboard.get" | "softkeyboard.apply" => view(),
        _ => Value::Null,
    }
}
