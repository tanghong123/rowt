//! Accumulate one VPN client's extract into a review file — a port of
//! `config/import-merge.py`.
//!
//! `rowt server import --from <client>` is run once per client, each run folding
//! that client's servers and subscriptions into ONE review file the human then
//! edits. So this is the piece that has to know what is already known: what is
//! in the pool (`servers.json` / `manual.json` / `subs.txt`), and what an
//! earlier run already accumulated. Everything kept is tagged `_source` so the
//! human — and `apply_import`, which strips it again — knows where it came from.
//!
//! Note the file this writes uses **`ensure_ascii=False`**, the opposite of
//! `sharelink`'s writer. Two files in the same pipeline disagreeing about that
//! is easy to "fix" by reusing the wrong one, and the result would escape every
//! CJK node name in a file a person is expected to read. Use
//! `serde_json::to_string_pretty` here and `pyjson::dumps` there.

use crate::pyurl;
use crate::sharelink::{key_of, strip};
use serde_json::{Map, Value};

/// `norm_sub` — a subscription URL reduced to its identity, so the same
/// subscription pasted with a display-only `name=` is not imported twice.
///
/// The scheme and host lowercase but the PATH does not, a trailing slash goes,
/// the fragment goes, and the query is re-encoded — which is not a no-op: a
/// bare `flag` becomes `flag=`, and `%2f` becomes `%2F`.
///
/// A URL that will not split at all (a bracketed host that is not IPv6, a
/// netloc NFKC would rewrite) falls back to the stripped input rather than
/// raising. That path is reachable, not defensive.
pub fn norm_sub(url: &str) -> String {
    let s = strip(url);
    let Ok(u) = pyurl::urlsplit(s) else { return s.to_string() };
    let q: Vec<(String, String)> = pyurl::parse_qsl(&u.query, true)
        .into_iter()
        .filter(|(k, _)| k.to_lowercase() != "name")
        .collect();
    pyurl::urlunsplit(
        &u.scheme.to_lowercase(),
        &u.netloc.to_lowercase(),
        u.path.trim_end_matches('/'),
        &pyurl::urlencode(&q),
        "",
    )
}

/// `_sub_url` — the url of a subscription entry, or "" for anything that is not
/// an object with one.
fn sub_url(s: &Value) -> String {
    match s.get("url") {
        Some(Value::String(u)) => u.clone(),
        // A non-string `url` makes the Python die in `.strip()`; treated as
        // absent here so a hand-edited review file is dropped, not fatal.
        _ => String::new(),
    }
}

fn arr(v: Option<&Value>) -> &[Value] {
    match v {
        Some(Value::Array(a)) => a,
        _ => &[],
    }
}

/// The subscription URLs in a `subs.txt`: one per line, blanks and comments out.
fn pool_sub_lines(text: &str) -> Vec<String> {
    crate::sharelink::splitlines(text)
        .iter()
        .map(|l| strip(l).to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| norm_sub(&l))
        .collect()
}

pub struct Merged {
    pub acc: Value,
    /// The one line the Python prints to stderr, which `cmd_import` passes
    /// straight through to the user's terminal.
    pub summary: String,
}

/// The merge. `pools` are the loaded `--pool` files (a failed load is `None`,
/// which is how the Python's `_load` reports a missing OR malformed one — so a
/// typo'd path silently disables pool dedup rather than failing); `pool_subs`
/// are the raw contents of the `--pool-subs` files.
pub fn merge(
    acc_in: Option<Value>,
    add: &Map<String, Value>,
    source: &str,
    pools: &[Option<Value>],
    pool_subs: &[String],
) -> Merged {
    let mut acc: Map<String, Value> = match acc_in {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    // Assigning an existing key keeps its position; a new one appends. That is
    // why the order below is the order a fresh file comes out in.
    for k in ["servers", "subscriptions", "proxy_domains"] {
        if !acc.get(k).is_some_and(Value::is_array) {
            acc.insert(k.into(), Value::Array(vec![]));
        }
    }
    if !acc.get("skipped").is_some_and(Value::is_object) {
        acc.insert("skipped".into(), Value::Object(Map::new()));
    }

    let mut pool_keys: Vec<String> = Vec::new();
    for p in pools.iter().flatten() {
        for o in arr(Some(p)) {
            if o.is_object() {
                pool_keys.push(key_of(o));
            }
        }
    }
    let pool_sub_keys: Vec<String> = pool_subs.iter().flat_map(|t| pool_sub_lines(t)).collect();

    // Prune what the pool already has — i.e. what a previous run applied.
    let servers: Vec<Value> = arr(acc.get("servers"))
        .iter()
        .filter(|o| o.is_object() && !pool_keys.contains(&key_of(o)))
        .cloned()
        .collect();
    let subs: Vec<Value> = arr(acc.get("subscriptions"))
        .iter()
        .filter(|s| {
            let u = sub_url(s);
            !u.is_empty() && !pool_sub_keys.contains(&norm_sub(&u))
        })
        .cloned()
        .collect();

    let mut have_keys = pool_keys.clone();
    have_keys.extend(servers.iter().map(key_of));
    let mut have_subs = pool_sub_keys.clone();
    have_subs.extend(subs.iter().map(|s| norm_sub(&sub_url(s))));

    let (mut added_s, mut dup_s, mut added_u, mut dup_u) = (0u32, 0u32, 0u32, 0u32);
    let mut servers = servers;
    for o in arr(add.get("servers")) {
        if !o.is_object() {
            continue;
        }
        let k = key_of(o);
        if have_keys.contains(&k) {
            dup_s += 1;
            continue;
        }
        have_keys.push(k);
        servers.push(tagged(o, source));
        added_s += 1;
    }
    let mut subs = subs;
    for s in arr(add.get("subscriptions")) {
        let url = sub_url(s);
        if url.is_empty() {
            continue;
        }
        let n = norm_sub(&url);
        if have_subs.contains(&n) {
            dup_u += 1;
            continue;
        }
        have_subs.push(n);
        subs.push(tagged(s, source));
        added_u += 1;
    }

    let mut domains: Vec<Value> = arr(acc.get("proxy_domains")).to_vec();
    for d in arr(add.get("proxy_domains")) {
        if !domains.contains(d) {
            domains.push(d.clone());
        }
    }

    let mut skipped: Map<String, Value> = match acc.get("skipped") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    if let Some(Value::Object(add_skipped)) = add.get("skipped") {
        for (k, v) in add_skipped {
            // Both halves must convert; the Python wraps the whole statement,
            // so a bad EXISTING count skips the update too.
            let cur = skipped.get(k).cloned().unwrap_or(Value::from(0));
            if let (Ok(a), Ok(b)) = (py_int(&cur), py_int(v)) {
                skipped.insert(k.clone(), Value::from(a + b));
            }
        }
    }

    acc.insert("servers".into(), Value::Array(servers));
    acc.insert("subscriptions".into(), Value::Array(subs));
    acc.insert("proxy_domains".into(), Value::Array(domains));
    acc.insert("skipped".into(), Value::Object(skipped));

    let src = if source.is_empty() { "source" } else { source };
    let mut summary = format!("{src}: +{added_s} server(s), +{added_u} subscription(s)");
    if dup_s > 0 || dup_u > 0 {
        summary.push_str(&format!("  (skipped {dup_s} dup server(s), {dup_u} dup sub(s))"));
    }
    Merged { acc: Value::Object(acc), summary }
}

/// `{**o, "_source": source} if source else o` — an existing `_source` is
/// overwritten IN PLACE, keeping its position, rather than moved to the end.
fn tagged(o: &Value, source: &str) -> Value {
    if source.is_empty() {
        return o.clone();
    }
    let mut m = match o {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    m.insert("_source".into(), Value::from(source));
    Value::Object(m)
}

/// `int(v)` for the skipped counters — anything that raises is a skip.
fn py_int(v: &Value) -> Result<i64, ()> {
    match v {
        Value::Bool(b) => Ok(*b as i64),
        Value::Number(n) => {
            Ok(n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0).trunc() as i64))
        }
        Value::String(s) => strip(s).parse::<i64>().map_err(|_| ()),
        _ => Err(()),
    }
}

/// `json.dumps(acc, indent=2, ensure_ascii=False) + "\n"`.
pub fn render(acc: &Value) -> String {
    format!("{}\n", serde_json::to_string_pretty(acc).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn srv(server: &str, uuid: &str, tag: &str) -> Value {
        json!({"type":"vless","server":server,"server_port":443,"uuid":uuid,"tag":tag})
    }
    fn run(acc: Option<Value>, add: Value, source: &str, pools: &[Option<Value>]) -> Value {
        let Value::Object(add) = add else { panic!("add must be an object") };
        merge(acc, &add, source, pools, &[]).acc
    }
    fn tags_of(acc: &Value, key: &str) -> Vec<String> {
        acc[key].as_array().unwrap().iter().map(|o| o["server"].as_str().unwrap().into()).collect()
    }

    // The six checks config/test_import_merge.py makes, kept here so the
    // Python's own checklist survives the port.

    #[test]
    fn a_fresh_accumulation_tags_everything_with_its_source() {
        let r = run(
            None,
            json!({"servers":[srv("a.com","u1","t"), srv("b.com","u2","t")],
                   "subscriptions":[{"url":"https://x/s"}]}),
            "src1",
            &[],
        );
        assert_eq!(r["servers"].as_array().unwrap().len(), 2);
        assert!(r["servers"].as_array().unwrap().iter().all(|s| s["_source"] == "src1"));
        assert_eq!(r["subscriptions"][0]["_source"], "src1");
    }

    #[test]
    fn re_adding_the_same_server_under_a_new_name_is_a_dup() {
        let r = run(None, json!({"servers":[srv("a.com","u1","t")]}), "src1", &[]);
        let r2 = run(
            Some(r),
            json!({"servers":[srv("a.com","u1","A-renamed")]}),
            "src2",
            &[],
        );
        assert_eq!(r2["servers"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_genuinely_new_server_from_a_second_source_is_appended_and_tagged() {
        let r = run(None, json!({"servers":[srv("a.com","u1","t")]}), "src1", &[]);
        let r3 = run(Some(r), json!({"servers":[srv("c.com","u3","t")]}), "src2", &[]);
        assert_eq!(tags_of(&r3, "servers"), ["a.com", "c.com"]);
        assert_eq!(r3["servers"][1]["_source"], "src2");
    }

    #[test]
    fn the_pool_wins_over_both_adding_and_keeping() {
        let base = run(
            None,
            json!({"servers":[srv("a.com","u1","t"), srv("b.com","u2","t")]}),
            "src1",
            &[],
        );
        // Already in the pool: not added.
        let r4 = run(
            Some(base.clone()),
            json!({"servers":[srv("c.com","u3","t")]}),
            "src2",
            &[Some(json!([srv("c.com", "u3", "t")]))],
        );
        assert_eq!(tags_of(&r4, "servers"), ["a.com", "b.com"]);
        // Now in the pool: PRUNED from what was already accumulated.
        let r5 = run(
            Some(base),
            json!({"servers":[]}),
            "src2",
            &[Some(json!([srv("a.com", "u1", "t")]))],
        );
        assert_eq!(tags_of(&r5, "servers"), ["b.com"]);
    }

    #[test]
    fn a_display_only_name_does_not_make_a_second_subscription() {
        let base = run(None, json!({"subscriptions":[{"url":"https://x/s?token=1"}]}), "src1", &[]);
        let r6 = run(
            Some(base),
            json!({"subscriptions":[{"url":"https://x/s?token=1&name=foo"}]}),
            "src2",
            &[],
        );
        assert_eq!(r6["subscriptions"].as_array().unwrap().len(), 1);
    }

    // Beyond the Python's checklist.

    #[test]
    fn norm_sub_lowercases_the_host_but_not_the_path() {
        assert_eq!(norm_sub("https://X.EXAMPLE/S/?token=1"), "https://x.example/S?token=1");
        assert_eq!(norm_sub("https://x/"), "https://x");
        assert_eq!(norm_sub("https://x/s#frag"), "https://x/s");
        // Re-encoding is not a no-op: a bare flag gains an equals sign.
        assert_eq!(norm_sub("https://x/s?flag"), "https://x/s?flag=");
        // Unsplittable input falls back to the stripped original.
        assert_eq!(norm_sub("  https://u@[bad]:443/s  "), "https://u@[bad]:443/s");
        assert_eq!(norm_sub("not a url"), "not a url");
    }

    #[test]
    fn a_broken_pool_path_silently_disables_dedup_rather_than_failing() {
        // `_load` returns None for a missing file AND for malformed JSON, so a
        // typo'd --pool is indistinguishable from an empty one.
        let r = run(None, json!({"servers":[srv("a.com","u1","t")]}), "src1", &[None]);
        assert_eq!(r["servers"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn skipped_counters_add_and_a_bad_one_is_left_alone() {
        let acc = json!({"servers":[],"subscriptions":[],"proxy_domains":[],
                         "skipped":{"ss":2,"bad":"x"}});
        let Value::Object(add) =
            json!({"skipped":{"ss":3,"bad":1,"new":"4"}}) else { unreachable!() };
        let out = merge(Some(acc), &add, "s", &[], &[]).acc;
        assert_eq!(out["skipped"]["ss"], 5);
        // The existing value cannot be int()'d, so the whole update is skipped.
        assert_eq!(out["skipped"]["bad"], "x");
        assert_eq!(out["skipped"]["new"], 4);
    }

    #[test]
    fn the_summary_counts_both_what_landed_and_what_was_a_dup() {
        let base = run(None, json!({"servers":[srv("a.com","u1","t")]}), "src1", &[]);
        let Value::Object(add) = json!({"servers":[srv("a.com","u1","t"), srv("z.com","u9","t")]})
        else {
            unreachable!()
        };
        let m = merge(Some(base), &add, "src2", &[], &[]);
        assert_eq!(
            m.summary,
            "src2: +1 server(s), +0 subscription(s)  (skipped 1 dup server(s), 0 dup sub(s))"
        );
        // No dups at all and the parenthetical is absent entirely.
        let m2 = merge(None, &add, "", &[], &[]);
        assert_eq!(m2.summary, "source: +2 server(s), +0 subscription(s)");
    }

    #[test]
    fn an_empty_source_leaves_the_entry_untagged() {
        let r = run(None, json!({"servers":[srv("a.com","u1","t")]}), "", &[]);
        assert!(r["servers"][0].get("_source").is_none());
    }
}
