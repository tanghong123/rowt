//! Which lane a destination takes on the main port, as the router decides it:
//! the verdict `rowt explain` prints and `rowt speed` labels each URL with.
//!
//! The hand lists are text and `rowt_core::classify` reads them. The geosite
//! rule-sets are binary, so the router's own engine is asked about them,
//! `sing-box rule-set match -f binary <set> <name>`, one cached set at a time
//! in the order `render_host` emits them: the escape lane's, the block lane's,
//! then the ad set, all after the hand lists and before the `resolve` rule.
//! Until #s96 both commands stopped at the hand lists, so a box whose escape
//! lane carries geosite:github (rowt's default list does) was told
//! registry.npmjs.org went DIRECT while the router sent it to escape: the
//! GitHub set lists npm's domains.

use crate::lifecycle::Ctx;
use crate::{env_or, PROG};
use rowt_core::classify::{classify, needs_resolution, normalize_dest, Classification, ClassifyInput, Lane};
use rowt_core::lanes::Lanes;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What the cached geosite sets say about one name.
#[derive(Debug, Default)]
pub struct GeoRoute {
    /// The first set that lists the name, and the lane it routes to.
    pub hit: Option<(String, Lane)>,
    /// Sets the router uses that sing-box could not read, in the order they
    /// were tried (so on a hit, only the ones ahead of it).
    pub unread: Vec<String>,
    /// Sets a lane names that are not cached. `render_host` leaves them out,
    /// so they route nothing until `rowt fetch host`.
    pub uncached: Vec<String>,
}

/// The cached sets in the router's order, each name once (a repeat can only
/// repeat a miss), and the named-but-uncached ones in file order.
fn sets(cfg: &Path, lanes: &Lanes) -> (Vec<(String, Lane, PathBuf)>, Vec<String>) {
    let cache = cfg.join("cache");
    let named = rowt_core::render::geosites_of(&lanes.escape).into_iter().map(|n| (n, Lane::Escape))
        .chain(rowt_core::render::geosites_of(&lanes.block).into_iter().map(|n| (n, Lane::Block)));
    let (mut out, mut uncached): (Vec<(String, Lane, PathBuf)>, Vec<String>) = (vec![], vec![]);
    for (n, l) in named {
        let srs = cache.join(format!("geosite-{n}.srs"));
        if !srs.is_file() {
            if !uncached.contains(&n) {
                uncached.push(n);
            }
        } else if !out.iter().any(|(m, _, _)| *m == n) {
            out.push((n, l, srs));
        }
    }
    let ads = cache.join("geosite-category-ads-all.srs");
    if ads.is_file() && !out.iter().any(|(m, _, _)| m == "category-ads-all") {
        out.push(("category-ads-all".into(), Lane::Block, ads));
    }
    (out, uncached)
}

enum Verdict {
    Match,
    Miss,
    Unread,
}

/// `rule-set match` exits 0 whether or not the set lists the name, and says
/// which on stderr ("match rules.[N]: …" or nothing). It exits non-zero only
/// when it cannot read the set.
fn rule_set_match(sb: &Path, srs: &Path, name: &str) -> Verdict {
    let out = Command::new(sb)
        .args(["rule-set", "match", "-f", "binary"])
        .arg(srs)
        .arg(name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            if String::from_utf8_lossy(&o.stderr).contains("match rules.") { Verdict::Match } else { Verdict::Miss }
        }
        _ => Verdict::Unread,
    }
}

/// Ask each cached set, in the router's order, about a name no hand list
/// matched. The first set that lists it is the router's answer.
pub fn geosite_route(ctx: &Ctx, lanes: &Lanes, name: &str) -> GeoRoute {
    let (sets, uncached) = sets(&ctx.cfg, lanes);
    let sb = ctx.sb();
    let mut unread: Vec<String> = vec![];
    for (n, l, srs) in sets {
        // No sing-box to ask is a set nobody could read.
        let v = if sb.is_file() { rule_set_match(&sb, &srs, name) } else { Verdict::Unread };
        match v {
            Verdict::Match => return GeoRoute { hit: Some((n, l)), unread, uncached },
            Verdict::Miss => {}
            Verdict::Unread => unread.push(n),
        }
    }
    GeoRoute { hit: None, unread, uncached }
}

/// The lane `dest` takes on the main port and why, with what the geosite sets
/// said (empty unless the name reached them).
pub fn route(ctx: &Ctx, lanes: &Lanes, dest: &str) -> (Classification, GeoRoute) {
    let mode = ctx.mode();
    let private: Vec<String> =
        ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "100.64.0.0/10", "169.254.0.0/16"]
            .iter().map(|s| s.to_string()).collect();
    let final_route = Lane::parse(&env_or("ROWT_FINAL", "direct")).unwrap_or(Lane::Direct);
    let pd = env_or("ROWT_PRIVATE_DEFAULT", "corp");
    // A name no hand list matched reaches the sets first; only one none of them
    // lists goes on to the router's lookup.
    let unlisted = needs_resolution(dest, &lanes.escape, &lanes.corp, &lanes.block);
    let geo = if unlisted { geosite_route(ctx, lanes, &normalize_dest(dest)) } else { GeoRoute::default() };
    // The resolver the router uses for a name no rule matched (the direct lane's
    // dns-direct: the public one in local mode), and only when the router
    // resolves such names at all. Asked exactly as the shell asks it
    // (`_captive_resolve_at`), and only on the branch that needs it.
    let resolver = if rowt_core::classify::router_resolves(final_route, &lanes.corp, &pd) {
        if mode == "local" { env_or("ROWT_DNS_LOCAL", "1.1.1.1") } else { env_or("ROWT_DNS_DIRECT", "223.5.5.5") }
    } else {
        String::new()
    };
    let ip = if !resolver.is_empty() && unlisted && geo.hit.is_none() {
        use rowt_platform::Platform;
        rowt_platform::Mac.resolve_at(&resolver, &normalize_dest(dest)).unwrap_or_default()
    } else {
        String::new()
    };
    let c = classify(dest, &ClassifyInput {
        escape_list: &lanes.escape, corp_list: &lanes.corp, block_list: &lanes.block,
        private_cidrs: &private, private_default: &pd, final_route,
        local_mode: mode == "local", resolved_ip: &ip, prog: PROG, resolver: &resolver,
        geosite: geo.hit.as_ref().map(|(n, l)| (n.as_str(), *l)),
    });
    (c, geo)
}

/// `explain`'s notes about the sets. On a hit: the sets ahead of it sing-box
/// could not read, since the router checks those first. Otherwise: the sets
/// it could not read, and the ones the router skips for want of a cache.
/// `fetch host` caches a set but does not re-render, so the router has it
/// only after a reload (`up` fetches and renders in one go).
pub fn geosite_notes(c: &Classification, geo: &GeoRoute) -> Vec<String> {
    let mut out = vec![];
    if geo.hit.is_some() {
        if !geo.unread.is_empty() {
            out.push(format!("  note:    the router checks geosite:{} first, and sing-box could not read it: that set may take this name instead", geo.unread.join(",")));
        }
        return out;
    }
    if c.lane != Lane::Block && !geo.unread.is_empty() {
        out.push(format!("  note:    a geosite rule-set may still match this (sing-box could not read it): geosite:{}", geo.unread.join(",")));
    }
    if !geo.uncached.is_empty() {
        out.push(format!("  note:    the router skips geosite:{} — named on a lane but not cached ('{PROG} fetch host' fetches it, then '{PROG} reload' puts it in the router)", geo.uncached.join(",")));
    }
    out
}
