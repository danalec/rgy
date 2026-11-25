// SPDX-License-Identifier: Apache-2.0

//! Which view of an adapter-claimed file a search reads: the file's own
//! bytes, or the rendering an adapter produced from it.
//!
//! The vendored rgx-core decides this from `RGX_ADAPTER_VIEW` -- rgx's own
//! name for the knob. rgy has a name of its own, `RGY_ADAPTER_VIEW`, and
//! the two are not the same product: a user who exports one of them is
//! configuring a tool, and a tool that answered to the other's variable
//! would change without being asked.
//!
//! So rgy reads its own name first and falls back to rgx's. The fallback is
//! the convenient half and the own name is the escape hatch: a shell that
//! exports `RGX_ADAPTER_VIEW=render` for rgx gets the same behaviour in
//! rgy, and a shell that exports `RGY_ADAPTER_VIEW` gets rgy to differ from
//! rgx without touching rgx.
//!
//! Nothing here changes the vendored tree. rgx-core exposes
//! [`rgx_core::set_policy`], so rgy resolves the value and hands it over.
//! With neither name set nothing is called and rgx-core's own resolution
//! applies, which answers `source` -- the conservative end, the only policy
//! that cannot lose a line.

/// rgy's own name for the knob.
pub const VIEW_ENV: &str = "RGY_ADAPTER_VIEW";

/// rgx's name for the same knob, read as a fallback.
pub const UPSTREAM_VIEW_ENV: &str = "RGX_ADAPTER_VIEW";

/// The policy a pair of values asks for.
///
/// `own` is rgy's name and wins; `upstream` is rgx's and is the fallback.
/// A value that is not one of the known ones is `Source`, the conservative
/// end, and it is `Source` even when the fallback would have said otherwise:
/// the name that was set is the name that counts, and a name set to
/// something unknown is not a reason to read the other one.
///
/// `None` means neither name was set, and the caller is left alone.
pub fn policy_from(own: Option<&str>, upstream: Option<&str>) -> Option<rgx_core::Policy> {
    let value = own.or(upstream)?.trim();
    Some(match value {
        "render" | "rendered" | "adapter" => rgx_core::Policy::Render,
        "both" | "union" => rgx_core::Policy::Both,
        _ => rgx_core::Policy::Source,
    })
}

/// The policy the environment asks for, or `None` when neither name is set.
#[cfg(feature = "std")]
pub fn resolve() -> Option<rgx_core::Policy> {
    let own = std::env::var(VIEW_ENV).ok();
    let upstream = std::env::var(UPSTREAM_VIEW_ENV).ok();
    policy_from(own.as_deref(), upstream.as_deref())
}

/// Apply the policy the environment asks for.
///
/// Called once, at startup, by the binaries that search. With neither name
/// set this does nothing, and rgx-core's own resolution stands.
#[cfg(feature = "std")]
pub fn apply() {
    let own = std::env::var(VIEW_ENV).ok();
    let upstream = std::env::var(UPSTREAM_VIEW_ENV).ok();
    apply_from(own.as_deref(), upstream.as_deref());
}

/// The same, from values rather than from the environment.
///
/// The values are the caller's: a caller that read them somewhere else
/// (a config file, a flag, a test) hands them over and the policy is the
/// same one the environment would have asked for.
pub fn apply_from(own: Option<&str>, upstream: Option<&str>) {
    if let Some(p) = policy_from(own, upstream) {
        rgx_core::set_policy(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neither_name_set_leaves_the_policy_alone() {
        assert_eq!(policy_from(None, None), None);
    }

    #[test]
    fn rgy_own_name_is_read() {
        assert_eq!(
            policy_from(Some("render"), None),
            Some(rgx_core::Policy::Render)
        );
        assert_eq!(
            policy_from(Some("both"), None),
            Some(rgx_core::Policy::Both)
        );
        assert_eq!(
            policy_from(Some("source"), None),
            Some(rgx_core::Policy::Source)
        );
    }

    #[test]
    fn rgx_name_is_the_fallback() {
        assert_eq!(
            policy_from(None, Some("render")),
            Some(rgx_core::Policy::Render)
        );
        assert_eq!(
            policy_from(None, Some("both")),
            Some(rgx_core::Policy::Both)
        );
    }

    #[test]
    fn rgy_own_name_wins_over_the_fallback() {
        assert_eq!(
            policy_from(Some("render"), Some("source")),
            Some(rgx_core::Policy::Render)
        );
        assert_eq!(
            policy_from(Some("source"), Some("render")),
            Some(rgx_core::Policy::Source)
        );
    }

    #[test]
    fn a_name_set_to_something_unknown_is_the_conservative_end() {
        // The fallback is not consulted: the name that was set is the name
        // that counts, and an unknown value is Source.
        assert_eq!(
            policy_from(Some("nonsense"), Some("render")),
            Some(rgx_core::Policy::Source)
        );
        assert_eq!(
            policy_from(None, Some("nonsense")),
            Some(rgx_core::Policy::Source)
        );
    }

    #[test]
    fn the_values_are_the_ones_the_vendored_parser_uses() {
        for v in ["render", "rendered", "adapter"] {
            assert_eq!(
                policy_from(Some(v), None),
                Some(rgx_core::Policy::Render),
                "{v} is a render policy"
            );
        }
        for v in ["both", "union"] {
            assert_eq!(
                policy_from(Some(v), None),
                Some(rgx_core::Policy::Both),
                "{v} is a both policy"
            );
        }
    }

    #[test]
    fn the_value_is_trimmed_like_the_vendored_parser_trims_it() {
        assert_eq!(
            policy_from(Some("  render \n"), None),
            Some(rgx_core::Policy::Render)
        );
    }

    #[test]
    fn applying_the_policy_reaches_the_vendored_library() {
        // The policy is process-wide and cached, so this leaves it set for
        // the rest of this test binary. Nothing else in the crate reads it,
        // and the last line puts it back where it started.
        apply_from(Some("render"), None);
        assert_eq!(rgx_core::policy(), rgx_core::Policy::Render);
        apply_from(Some("both"), None);
        assert_eq!(rgx_core::policy(), rgx_core::Policy::Both);
        apply_from(Some("source"), None);
        assert_eq!(rgx_core::policy(), rgx_core::Policy::Source);
    }
}
