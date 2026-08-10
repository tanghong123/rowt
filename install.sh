#!/usr/bin/env bash
#
# install.sh — install (or update) the rowt tool.
#
# Copies the tool to a prefix and symlinks `rowt` into a bin dir on your PATH,
# and wires rowt's shell integration into ~/.zshrc via `eval "$(rowt shell-init)"`
# — the `rowt-proxy-on` / `rowt-proxy-off` aliases plus subcommand tab-completion.
# Idempotent: re-running does nothing if the installed copy is same-or-newer and
# the integration is already present. Version-guarded (pass --force to override).
#
#   ./install.sh                 install/update into the defaults below
#   ./install.sh --force         reinstall even if installed >= source
#   ./install.sh --uninstall     remove the installed copy + symlink
#   ./install.sh --prefix DIR    where the tool is copied (default ~/.local/share/rowt)
#   ./install.sh --bindir DIR    where the `rowt` symlink goes  (default ~/.local/bin)
set -euo pipefail

HERE="$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd)"          # repo rowt/
PREFIX="${ROWT_PREFIX:-$HOME/.local/share/rowt}"
BINDIR="${ROWT_BINDIR:-$HOME/.local/bin}"
ZSHRC="${ROWT_ZSHRC:-${ZDOTDIR:-$HOME}/.zshrc}"
ALIAS_BEGIN="# >>> rowt aliases >>>"
ALIAS_END="# <<< rowt aliases <<<"
FORCE=0
UNINSTALL=0

err() { echo "error: $*" >&2; }
die() { err "$*"; exit 1; }

# Idempotently wire rowt's shell integration into the rc (marker block, added at
# most once). We source the blessed 'rowt shell-init' rather than hard-coding the
# aliases, so a single line stays current across versions and also loads the
# subcommand tab-completion (rowt 1.3.0+). If an older static-alias block is
# present, migrate it in place.
add_shell_aliases() {
  [ -f "$ZSHRC" ] || touch "$ZSHRC"
  if grep -qF "$ALIAS_BEGIN" "$ZSHRC"; then
    grep -qF 'rowt shell-init' "$ZSHRC" && return 0  # current block already present
    remove_shell_aliases                             # stale static block -> migrate below
  fi
  cat >> "$ZSHRC" <<'ROWTALIASES'

# >>> rowt aliases >>>
# rowt shell integration: rowt-proxy-on/-off aliases + subcommand tab-completion
command -v rowt >/dev/null 2>&1 && eval "$(rowt shell-init)"
# <<< rowt aliases <<<
ROWTALIASES
  echo "wired 'rowt shell-init' (proxy aliases + tab-completion) into $ZSHRC (run 'source $ZSHRC' or open a new shell)"
}

remove_shell_aliases() {
  [ -f "$ZSHRC" ] || return 0
  if grep -qF "$ALIAS_BEGIN" "$ZSHRC"; then
    sed -i.rowtbak "/$ALIAS_BEGIN/,/$ALIAS_END/d" "$ZSHRC" && rm -f "$ZSHRC.rowtbak"
    echo "removed rowt aliases from $ZSHRC"
  fi
}

# If rowt's router is running, make sure the system proxy + *.local / private
# bypass are in place (this is what fixes an active mDNS retry storm). We ask
# rowt whether it's ALREADY configured first (a sudo-free read), and only invoke
# the admin path / restart when something actually needs fixing — so re-running
# install.sh never prompts for a password when nothing changed.
reapply_proxy_if_active() {
  local rowt="$LINK"; [ -x "$rowt" ] || rowt="$PREFIX/bin/rowt"; [ -x "$rowt" ] || return 0
  local pidf="${XDG_CONFIG_HOME:-$HOME/.config}/rowt/host.pid" pid
  pid="$(cat "$pidf" 2>/dev/null || true)"
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || return 0     # not running -> nothing to do
  if "$rowt" proxy check >/dev/null 2>&1; then
    echo "rowt running; system proxy already configured (local bypass set) — no admin needed."
    return 0
  fi
  echo "rowt is running but the system proxy/bypass needs fixing — applying (needs admin)…"
  "$rowt" proxy on || echo "  (could not set proxy automatically — run 'rowt proxy on' yourself)"
  # restart so the bypass takes effect cleanly AND any accumulated spin clears
  echo "restarting the router…"
  "$rowt" router restart >/dev/null 2>&1 || true
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --force) FORCE=1 ;;
    --uninstall) UNINSTALL=1 ;;
    --prefix) PREFIX="${2:?}"; shift ;;
    --bindir) BINDIR="${2:?}"; shift ;;
    -h|--help) sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
  shift
done

LINK="$BINDIR/rowt"

# version helpers ($1 >= $2 ?)
ver_of() { "$1" version 2>/dev/null | awk '{print $NF}'; }
ver_ge() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)" = "$1" ]; }

if [ "$UNINSTALL" -eq 1 ]; then
  [ -L "$LINK" ] && rm -f "$LINK" && echo "removed symlink $LINK" || true
  [ -d "$PREFIX" ] && rm -rf "$PREFIX" && echo "removed $PREFIX" || true
  remove_shell_aliases
  echo "uninstalled."
  exit 0
fi

# ensure shell aliases on every install/update run (idempotent, so this is safe
# even on a same-version re-run that otherwise short-circuits below)
add_shell_aliases

src_ver="$(ver_of "$HERE/bin/rowt")"
[ -n "$src_ver" ] || die "cannot read source version"

if [ -x "$PREFIX/bin/rowt" ]; then
  inst_ver="$(ver_of "$PREFIX/bin/rowt")"
  if [ -n "$inst_ver" ] && [ "$FORCE" -ne 1 ] && ver_ge "$inst_ver" "$src_ver"; then
    if [ "$inst_ver" = "$src_ver" ]; then
      echo "rowt $inst_ver already installed at $PREFIX — nothing to do."
    else
      echo "installed rowt $inst_ver is newer than source $src_ver — not overriding (use --force)."
    fi
    reapply_proxy_if_active   # still (re)apply the proxy bypass even on a same-version re-run
    exit 0
  fi
  echo "updating rowt $inst_ver -> $src_ver"
else
  echo "installing rowt $src_ver -> $PREFIX"
fi

# copy the tool (exclude any local state/binaries that shouldn't ship)
#
# The two `target/` trees are excluded because they are BUILD output, not the
# tool: measured 2026-08-10 they are 406 MB and 774 MB of object files, and
# copying them made a local install 1.2 GB. They were also, until this change,
# how a local install found its Rust sidecars — `_render_bin` falls back to
# `$HERE/target/release` and `_collector_bin` to `$HERE/rowt-monitor/target/
# release`. So the exclusion and the explicit install below are ONE change:
# excluding alone would take the sidecars with it.
mkdir -p "$PREFIX" "$BINDIR"
if command -v rsync >/dev/null 2>&1; then
  rsync -a --delete --exclude '.state' --exclude 'bin/sing-box' \
        --exclude 'target' --exclude 'rowt-monitor/target' "$HERE/" "$PREFIX/"
else
  rm -rf "$PREFIX"; mkdir -p "$PREFIX"
  # `cp -R` has no --exclude, so copy the entries and skip the build trees.
  for e in "$HERE"/* "$HERE"/.[!.]*; do
    case "${e##*/}" in target|.state) continue ;; esac
    cp -R "$e" "$PREFIX/" 2>/dev/null || true
  done
  rm -rf "$PREFIX/rowt-monitor/target" "$PREFIX/bin/sing-box"
fi

# The Rust sidecars, into the one place bin/rowt looks first ($HERE/bin). They
# are copied rather than found in target/ so the installed tree does not depend
# on a build directory that may be rebuilt, cleaned, or absent.
#
# `rowt-render` and `rowt-watch-tick` are INERT unless ROWT_RENDER_SHADOW=1 or
# ROWT_WATCH_SHADOW=1: bin/rowt stays authoritative and only compares. Shipping
# them is what makes the shadow window startable at all, and is not a cutover.
# `rowt-rust` is the Rust CLI, installed under its own name so it can be run
# side by side; nothing execs it.
for b in rowt-render rowt-watch-tick; do
  [ -x "$HERE/target/release/$b" ] && cp "$HERE/target/release/$b" "$PREFIX/bin/$b" || true
done
[ -x "$HERE/rowt-monitor/target/release/rowt-collector" ] \
  && cp "$HERE/rowt-monitor/target/release/rowt-collector" "$PREFIX/bin/rowt-collector" || true
[ -x "$HERE/rowt-monitor/target/release/rowt-monitor" ] \
  && cp "$HERE/rowt-monitor/target/release/rowt-monitor" "$PREFIX/bin/rowt-monitor" || true
if [ -x "$HERE/target/release/rowt-rs" ]; then
  cp "$HERE/target/release/rowt-rs" "$PREFIX/bin/rowt-rust"
  ln -sfn "$PREFIX/bin/rowt-rust" "$BINDIR/rowt-rust"
fi

ln -sfn "$PREFIX/bin/rowt" "$LINK"
chmod +x "$PREFIX/bin/rowt"

echo "installed: $LINK -> $PREFIX/bin/rowt  (rowt $src_ver)"
case ":$PATH:" in
  *":$BINDIR:"*) ;;
  *) echo "note: $BINDIR is not on your PATH — add it, e.g.:"
     echo "  echo 'export PATH=\"$BINDIR:\$PATH\"' >> ~/.zshrc" ;;
esac
reapply_proxy_if_active   # apply the *.local bypass now if rowt is running
echo "run:  rowt help"
echo "shell aliases: rowt-proxy-on / rowt-proxy-off  (new shell or 'source $ZSHRC')"
