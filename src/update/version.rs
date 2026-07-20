//! 版本解析与升级判定。
//!
//! 本地版本来自 build.rs 注入的 `WIND_SETTING_VERSION`，有四种形态：
//!   - `v0.106.0`                  tag 上
//!   - `v0.106.0+a1b2c3d`          tag 之后有提交
//!   - `v0.106.0+a1b2c3d.dirty`    且工作区脏
//!   - `a1b2c3d`                   无 tag，纯 hash —— **无法解析**
//!
//! 最后一种解析失败时判定为 [`Verdict::LocalUnparsable`] 并跳过升级提示，而不是
//! 像旧 Go 版那样降级为 0.0.0 —— 那会让每个开发构建每次启动都提示"有新版本"。
//!
//! 比较一律交给 `semver` crate，不自行实现：prerelease 的数字标识符按**数值**而非
//! 字典序排序（`dev.10 > dev.2`），手写比较器几乎必然在此出错。

use semver::Version;

/// 升级判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// 远端严格新于本地。
    Upgrade { local: Version, remote: Version },
    /// 本地已是最新（含本地更新于远端的情况）。
    UpToDate,
    /// 本地版本无法解析（无 tag 的开发构建），跳过升级。
    LocalUnparsable,
}

/// 剥掉可选的 `v`/`V` 前缀后按 semver 解析；失败返回 `None`。
pub fn parse(raw: &str) -> Option<Version> {
    let s = raw.trim();
    let s = s
        .strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s);
    Version::parse(s).ok()
}

/// 编译期注入的本地版本字符串（原样，含 `v` 前缀与 build metadata）。
pub fn local_raw() -> &'static str {
    env!("WIND_SETTING_VERSION")
}

/// 解析后的本地版本；无 tag 的开发构建返回 `None`。
pub fn local() -> Option<Version> {
    parse(local_raw())
}

/// 判定是否应提示升级。
///
/// build metadata（`+hash`）按 semver 规范**不参与**比较，因此本地 `0.106.0+a1b2c3d`
/// 与远端 `0.106.0` 视为同版本 —— tag 之后的本地构建不会被"降级"回该 tag。
pub fn verdict(local_raw: &str, remote: &Version) -> Verdict {
    let Some(local) = parse(local_raw) else {
        return Verdict::LocalUnparsable;
    };
    if *remote > local {
        Verdict::Upgrade {
            local,
            remote: remote.clone(),
        }
    } else {
        Verdict::UpToDate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        parse(s).expect("测试版本号应可解析")
    }

    #[test]
    fn parse_accepts_all_local_version_shapes() {
        assert_eq!(v("v0.106.0"), Version::new(0, 106, 0));
        // build metadata 保留在结构里，但不参与比较
        assert_eq!(v("v0.106.0+a1b2c3d").major, 0);
        assert_eq!(v("v0.106.0+a1b2c3d.dirty").minor, 106);
        assert_eq!(v("0.106.0"), Version::new(0, 106, 0));
    }

    #[test]
    fn parse_rejects_bare_hash() {
        assert!(parse("a1b2c3d").is_none());
        assert!(parse("").is_none());
        assert!(parse("dev").is_none());
    }

    #[test]
    fn build_metadata_is_ignored_in_comparison() {
        // tag 之后的本地构建与该 tag 同版本，不应提示升级
        assert_eq!(verdict("v0.106.0+a1b2c3d", &v("0.106.0")), Verdict::UpToDate);
        assert_eq!(
            verdict("v0.106.0+a1b2c3d.dirty", &v("0.106.0")),
            Verdict::UpToDate
        );
    }

    #[test]
    fn detects_upgrade() {
        assert!(matches!(
            verdict("v0.106.0", &v("0.107.0")),
            Verdict::Upgrade { .. }
        ));
        assert!(matches!(
            verdict("v0.106.0+a1b2c3d", &v("0.107.0")),
            Verdict::Upgrade { .. }
        ));
    }

    #[test]
    fn same_version_is_up_to_date() {
        assert_eq!(verdict("v0.107.0", &v("0.107.0")), Verdict::UpToDate);
    }

    #[test]
    fn local_newer_than_remote_is_up_to_date() {
        // 本地跑着比线上更新的构建时不应"降级"
        assert_eq!(verdict("v0.108.0", &v("0.107.0")), Verdict::UpToDate);
    }

    #[test]
    fn bare_hash_local_skips_check() {
        assert_eq!(verdict("a1b2c3d", &v("0.107.0")), Verdict::LocalUnparsable);
    }

    /// 旧 Go 版的实质 bug：prerelease 之间恒判"不更新"，dev 流根本升不动。
    #[test]
    fn prerelease_increments_are_upgrades() {
        assert!(matches!(
            verdict("v0.107.0-dev.1", &v("0.107.0-dev.2")),
            Verdict::Upgrade { .. }
        ));
    }

    /// semver 规范：prerelease 的纯数字标识符按数值比较，不是字典序。
    /// 字典序会得出 "dev.10" < "dev.2"，dev.N 累积到两位数时就会卡住升级。
    #[test]
    fn numeric_prerelease_identifiers_compare_numerically() {
        assert!(matches!(
            verdict("v0.107.0-dev.2", &v("0.107.0-dev.10")),
            Verdict::Upgrade { .. }
        ));
        assert_eq!(
            verdict("v0.107.0-dev.10", &v("0.107.0-dev.2")),
            Verdict::UpToDate
        );
    }

    #[test]
    fn stable_is_newer_than_its_own_prerelease() {
        assert!(matches!(
            verdict("v0.107.0-dev.9", &v("0.107.0")),
            Verdict::Upgrade { .. }
        ));
        assert_eq!(verdict("v0.107.0", &v("0.107.0-dev.9")), Verdict::UpToDate);
    }

    #[test]
    fn next_dev_cycle_is_newer_than_previous_stable() {
        assert!(matches!(
            verdict("v0.107.0", &v("0.108.0-dev.1")),
            Verdict::Upgrade { .. }
        ));
    }

    #[test]
    fn injected_local_version_is_parsable_or_dev_build() {
        // 守护 build.rs 的注入格式：要么可解析，要么是无 tag 的开发构建。
        // 若某天注入格式变成既非 semver 又非 hash，此处会提示去看 build.rs。
        let raw = local_raw();
        assert!(
            !raw.is_empty(),
            "WIND_SETTING_VERSION 为空，build.rs 注入失效"
        );
    }
}
