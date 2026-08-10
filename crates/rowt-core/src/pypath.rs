//! `PurePosixPath` — its string form and its sort order.
//!
//! Two places in `foreign-import.py` are load-bearing:
//!
//!   * `print(f"reading {source} from {root}")` prints `str(Path(...))`, which
//!     is NOT the string you passed to `--path`: `/tmp/x/` comes back `/tmp/x`,
//!     `./a` comes back `a`, and `""` comes back `.`.
//!   * `sorted(root.rglob("*.y*ml"))` orders by `_parts_normcase`, i.e. by the
//!     normalized string SPLIT ON `/` and compared component by component — not
//!     by the string. `a/b.yaml` sorts before `a-x/c.yaml` because `a < a-x`,
//!     while as plain strings `a-x/c.yaml` wins (`-` is 0x2D, `/` is 0x2F).
//!     Get that backwards and the servers come out of a profile directory in a
//!     different order, which is a different `server-N` for every unnamed one.

/// `str(PurePosixPath(s))`.
///
/// Empty and `.` components go, runs of slashes collapse, `..` stays (a pure
/// path never resolves), and a path that begins with EXACTLY two slashes keeps
/// both — POSIX reserves `//host/path`, three or more do not qualify.
pub fn path_str(s: &str) -> String {
    let root = if s.starts_with("//") && !s.starts_with("///") {
        "//"
    } else if s.starts_with('/') {
        "/"
    } else {
        ""
    };
    let tail: Vec<&str> = s.split('/').filter(|c| !c.is_empty() && *c != ".").collect();
    if root.is_empty() && tail.is_empty() {
        return ".".into();
    }
    format!("{root}{}", tail.join("/"))
}

/// `path.name` — the last component, or "" for a path that is all root.
pub fn name(s: &str) -> String {
    let p = path_str(s);
    match p.rsplit('/').next() {
        Some(n) if n != "." && !n.is_empty() => n.to_string(),
        _ => String::new(),
    }
}

/// `PurePath.__lt__` — compare `str(path).split("/")` element by element. On
/// POSIX `_parts_normcase` is the parts as they are; case folding is a Windows
/// concern.
pub fn cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (pa, pb) = (path_str(a), path_str(b));
    let (va, vb): (Vec<&str>, Vec<&str>) = (pa.split('/').collect(), pb.split('/').collect());
    va.cmp(&vb)
}

/// `sorted(paths)`.
pub fn sort(paths: &mut [String]) {
    paths.sort_by(|a, b| cmp(a, b));
}

/// `fnmatch` for the one pattern the importers use, `*.y*ml`, applied to a
/// single path component. `*` matches any run INCLUDING an empty one and
/// including a leading dot — `pathlib`'s globs see hidden files, unlike
/// `glob.glob`'s.
pub fn matches_yaml(name: &str) -> bool {
    // `.*\.y.*ml` anchored: find a `.y` such that the rest ends in `ml`.
    let b = name.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'.' && b.get(i + 1) == Some(&b'y') {
            let rest = &name[i + 2..];
            if rest.len() >= 2 && rest.ends_with("ml") {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_normalizes_the_way_pathlib_does() {
        assert_eq!(path_str("//a/b"), "//a/b");
        assert_eq!(path_str("///a"), "/a");
        assert_eq!(path_str("a//b"), "a/b");
        assert_eq!(path_str("./a"), "a");
        assert_eq!(path_str("a/"), "a");
        assert_eq!(path_str(""), ".");
        assert_eq!(path_str("a/../b"), "a/../b");
        assert_eq!(path_str("a/./b"), "a/b");
        assert_eq!(path_str("/tmp/x/"), "/tmp/x");
        assert_eq!(path_str("/"), "/");
    }

    #[test]
    fn sort_is_by_component_not_by_string() {
        let mut v: Vec<String> = ["gt/a-x/c.yaml", "gt/a/b.yaml", "gt/.hid/d.yaml", "gt/.dot.yaml"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        sort(&mut v);
        assert_eq!(v, ["gt/.dot.yaml", "gt/.hid/d.yaml", "gt/a/b.yaml", "gt/a-x/c.yaml"]);
        // The trap: as plain strings `gt/a-x/…` would come first.
        let mut s = v.clone();
        s.sort();
        assert_ne!(s, v);
    }

    #[test]
    fn the_yaml_pattern_takes_dotfiles_and_needs_a_dot_y() {
        assert!(matches_yaml("a.yaml"));
        assert!(matches_yaml("a.yml"));
        assert!(matches_yaml(".dot.yaml"));
        assert!(matches_yaml("x.yanythingml"));
        // `*` matches empty, so the name may be nothing but the extension.
        assert!(matches_yaml(".yml"));
        assert!(!matches_yaml("a.yaml.txt"));
        assert!(!matches_yaml("ayml"));
        assert!(!matches_yaml("a.ym"));
    }

    #[test]
    fn name_is_the_last_component() {
        assert_eq!(name("/a/b/c.yaml"), "c.yaml");
        assert_eq!(name("c.yaml"), "c.yaml");
        assert_eq!(name("/"), "");
        assert_eq!(name(""), "");
    }
}
