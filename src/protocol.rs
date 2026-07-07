//! `windinput://` 协议解析 + 注册。
//! 格式:`windinput://import/<kind>?url=<https>&name=<可选>`,kind∈theme/schema/dict/extdict。

/// 协议导入请求(解析结果)。
pub struct ProtocolRequest {
    pub kind: String,
    pub url: String,
    pub name: String,
}

const KINDS: &[&str] = &["theme", "schema", "dict", "extdict"];

/// 解析 + 校验协议 URL。非法返回 Err(原因)。
pub fn parse_protocol_url(raw: &str) -> Result<ProtocolRequest, String> {
    let raw = raw.trim();
    let rest = raw
        .strip_prefix("windinput://")
        .or_else(|| raw.strip_prefix("WINDINPUT://"))
        .ok_or("不支持的协议(应为 windinput://)")?;
    // rest = "import/<kind>?<query>"
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, q),
        None => (rest, ""),
    };
    let mut segs = path.splitn(2, '/');
    let action = segs.next().unwrap_or("");
    if !action.eq_ignore_ascii_case("import") {
        return Err(format!("不支持的操作: {action}"));
    }
    let kind = segs
        .next()
        .unwrap_or("")
        .trim_matches('/')
        .to_ascii_lowercase();
    if !KINDS.contains(&kind.as_str()) {
        return Err(format!("不支持的导入类型: {kind}"));
    }
    let (mut url, mut name) = (String::new(), String::new());
    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            let val = percent_decode(v);
            match k {
                "url" => url = val,
                "name" => name = val,
                _ => {}
            }
        }
    }
    if url.is_empty() {
        return Err("缺少 url 参数".into());
    }
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err("url 参数必须是 https 链接".into());
    }
    Ok(ProtocolRequest { kind, url, name })
}

/// 最小百分号解码(`%XX` → 字节),非法序列原样保留。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 启动时自愈 `windinput://` 协议注册(仅 Windows)。
/// `HKCU\Software\Classes\windinput\shell\open\command = "<exe>" "%1"`。
#[cfg(windows)]
pub fn self_heal_registration() {
    use winreg::RegKey;
    use winreg::enums::*;

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let exe = exe.to_string_lossy().to_string();
    let want_cmd = format!("\"{exe}\" \"%1\"");
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    // 已正确则跳过
    if let Ok(cmd_key) = hkcu.open_subkey(r"Software\Classes\windinput\shell\open\command")
        && let Ok(cur) = cmd_key.get_value::<String, _>("")
        && cur.eq_ignore_ascii_case(&want_cmd)
    {
        return;
    }
    let Ok((root, _)) = hkcu.create_subkey(r"Software\Classes\windinput") else {
        return;
    };
    let _ = root.set_value("", &"URL:清风输入法协议");
    let _ = root.set_value("URL Protocol", &"");
    if let Ok((cmd, _)) = hkcu.create_subkey(r"Software\Classes\windinput\shell\open\command") {
        let _ = cmd.set_value("", &want_cmd);
    }
    tracing::info!("windinput:// 协议已注册/修复");
}

/// 非 Windows:协议随 .app 的 Info.plist 注册,无运行时代码。
#[cfg(not(windows))]
pub fn self_heal_registration() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_theme() {
        let r = parse_protocol_url(
            "windinput://import/theme?url=https%3A%2F%2Fsite.com%2Fa.yaml&name=Dark",
        )
        .unwrap();
        assert_eq!(r.kind, "theme");
        assert_eq!(r.url, "https://site.com/a.yaml");
        assert_eq!(r.name, "Dark");
    }

    #[test]
    fn parse_theme_import_https() {
        let req = parse_protocol_url(
            "windinput://import/theme?url=https://example.com/theme.toml&name=Blue",
        )
        .unwrap();
        assert_eq!(req.kind, "theme");
        assert_eq!(req.url, "https://example.com/theme.toml");
        assert_eq!(req.name, "Blue");
    }

    #[test]
    fn reject_protocol_import_http_url() {
        assert!(
            parse_protocol_url("windinput://import/theme?url=http://example.com/theme.toml")
                .is_err()
        );
    }

    #[test]
    fn rejects() {
        assert!(parse_protocol_url("http://import/theme?url=https://x").is_err()); // 错 scheme
        assert!(parse_protocol_url("windinput://import/unknown?url=https://x").is_err()); // 未知 kind
        assert!(parse_protocol_url("windinput://import/theme").is_err()); // 缺 url
        assert!(parse_protocol_url("windinput://import/theme?url=http://x/a.yaml").is_err());
        // 非 https
    }
}
