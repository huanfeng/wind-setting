//! 协议 `windinput://import/schema` 的方案包下载：仅官方域名 + 限额下载到临时文件。
//!
//! 下载机制照抄 `theme_import`（ureq 同步请求，超时 + 体积上限），但方案包是
//! 二进制 zip，限额放宽到 64 MB / 30s。下载成功后由 main 的协议路由转交
//! `scheme.previewImport` + schema_manager 既有确认对话框，本模块不做导入语义。

use std::io::Read;
use std::path::PathBuf;

/// 方案包体积上限（zip 内含词库，比主题大得多）。
const MAX_SCHEMA_BYTES: usize = 64 * 1024 * 1024;
/// 下载超时。
const DOWNLOAD_TIMEOUT_SECS: u64 = 30;

/// 官方域判定：`windinput.com` 本域或其任意子域（大小写不敏感）。
///
/// 子域必须以 `.windinput.com` **带点后缀**锚定 —— 只比 `ends_with("windinput.com")`
/// 会放过 `evil-windinput.com` 这类蹭名域。端口不在这里处理：调用方先从 URL
/// 解析出纯 host 再来问。
pub fn is_official_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "windinput.com" || host.ends_with(".windinput.com")
}

/// 从 https URL 中提取 host（小写，去端口、去 userinfo）。非 https 或无 host 返回 None。
///
/// 手写解析（本仓无 url crate）：authority = `https://` 与首个 `/?#` 之间；
/// 再剥掉 `user@` 前缀与 `:port` 后缀。IPv6 字面量带 `[]`，官方域用不到，直接判非法。
pub fn https_host_of(url: &str) -> Option<String> {
    let t = url.trim();
    // `get(..8)`：位置 8 落在多字节字符中间时返回 None 而不是 panic——URL 来自
    // 协议链接，是攻击者可控输入，绝不能因畸形串崩掉设置程序。
    let rest = t
        .get(..8)
        .filter(|p| p.eq_ignore_ascii_case("https://"))
        .map(|_| &t[8..])
        .filter(|r| !r.is_empty())?;
    let authority = rest.split(['/', '?', '#']).next()?;
    // userinfo：@ 之后才是 host（`https://user@evil.com/` 不能把 user 当 host）。
    let host_port = authority.rsplit('@').next()?;
    if host_port.starts_with('[') {
        return None;
    }
    let host = host_port.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// 校验官方域并下载方案包到临时文件（`%TEMP%\windinput_schema_import.zip`）。
///
/// 固定文件名：新下载覆盖旧残留，至多留一个文件在系统临时目录；清理交给系统
/// （确认对话框是异步的，文件生命周期要跨到导入完成，见 main 协议路由处说明）。
pub fn download_schema_package(url: &str) -> Result<PathBuf, String> {
    let host = https_host_of(url).ok_or_else(|| "方案包链接必须是 https://".to_string())?;
    if !is_official_host(&host) {
        return Err("方案包链接必须来自 windinput.com 官方域名".to_string());
    }
    let response = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .redirects(3)
        .build()
        .get(url)
        .call()
        .map_err(|e| format!("下载方案包失败：{e}"))?;
    let mut reader = response.into_reader().take((MAX_SCHEMA_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取方案包响应失败：{e}"))?;
    if bytes.len() > MAX_SCHEMA_BYTES {
        return Err("方案包超过 64 MB 限制".to_string());
    }
    let path = std::env::temp_dir().join("windinput_schema_import.zip");
    std::fs::write(&path, &bytes).map_err(|e| format!("写入临时文件失败：{e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_host_accepts_bare_and_subdomains_case_insensitive() {
        assert!(is_official_host("windinput.com"));
        assert!(is_official_host("WindInput.COM"));
        assert!(is_official_host("dl.windinput.com"));
        assert!(is_official_host("a.b.WINDINPUT.com"));
    }

    /// 蹭名域必须被点后缀锚定拦下——这是白名单存在的意义。
    #[test]
    fn official_host_rejects_lookalike_and_foreign_domains() {
        assert!(!is_official_host("evil-windinput.com"));
        assert!(!is_official_host("windinput.com.evil.com"));
        assert!(!is_official_host("windinput.org"));
        assert!(!is_official_host("example.com"));
        assert!(!is_official_host(""));
    }

    #[test]
    fn host_extraction_strips_port_userinfo_and_path() {
        assert_eq!(
            https_host_of("https://dl.windinput.com/pkg.zip").as_deref(),
            Some("dl.windinput.com")
        );
        assert_eq!(
            https_host_of("https://Windinput.Com:8443/a?b=c#d").as_deref(),
            Some("windinput.com"),
            "端口不参与域判定，host 归一为小写"
        );
        // userinfo 伪装：host 必须取 @ 之后的部分。
        assert_eq!(
            https_host_of("https://windinput.com@evil.com/pkg.zip").as_deref(),
            Some("evil.com")
        );
        assert_eq!(https_host_of("http://windinput.com/a.zip"), None);
        assert_eq!(https_host_of("https:///no-host"), None);
        assert_eq!(https_host_of("https://[::1]/x"), None);
    }

    /// 畸形串不 panic：多字节字符跨越第 8 字节（`https:/汉` 的「汉」占字节 7..10）
    /// 时按字节切片会崩——URL 是协议链接来的攻击者可控输入。
    #[test]
    fn host_extraction_survives_multibyte_garbage() {
        assert_eq!(https_host_of("https:/汉字"), None);
        assert_eq!(https_host_of("汉https://x"), None);
        assert_eq!(https_host_of("https://"), None);
        // IDN 子域本身合法解析（是否官方域由 is_official_host 另判）。
        assert_eq!(
            https_host_of("https://例子.windinput.com/x").as_deref(),
            Some("例子.windinput.com")
        );
    }

    /// 白名单校验发生在任何网络请求之前——非官方域直接拒绝，不发请求。
    #[test]
    fn download_rejects_non_official_before_request() {
        let err = download_schema_package("https://evil-windinput.com/pkg.zip").unwrap_err();
        assert!(err.contains("官方域名"), "{err}");
        let err = download_schema_package("http://windinput.com/pkg.zip").unwrap_err();
        assert!(err.contains("https"), "{err}");
    }
}
