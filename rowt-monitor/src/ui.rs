//! The renderer. Draws into a ratatui `Buffer` with absolute cell writes so the
//! output matches the golden `renders/*.ansi` byte-for-byte in width. Column
//! offsets below were measured from those renders.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::app::{App, Focus};
use crate::model::{ErrCat, Lane, Window};
use crate::paint::{dw, hfill, put, put_right, truncate};
use crate::{format, theme};

/// Clickable regions from the last draw, for mouse hit-testing.
#[derive(Clone, Default)]
pub struct Hit {
    pub side_by_side: bool,
    pub conn_list: Rect,
    pub err_list: Rect,
    pub conn_pane: Rect,
    pub err_pane: Rect,
    pub conn_h: usize,
    pub err_h: usize,
    pub lanes: Vec<(Rect, Option<Lane>)>, // header rate rows (None = `all`)
    pub windows: Vec<(Rect, Window)>,     // errors window tabs
    pub strip_w: u16,                     // server-strip viewport width (§5.4)
    pub strip_render_off: usize,          // marquee cell offset actually drawn (freeze to it → no jump)
    pub chips: Vec<(Rect, usize)>,        // server chips as drawn this frame (click to select)
    pub sysproxy: Rect,                   // the "sys proxy on/off" cell region (click to toggle)
}

// NOTE: the bottom row is shifted one space left of the design capture so its
// stems line up under the rows above (per user correction); the golden test
// masks the logo columns so this override doesn't fail the layout diff.
const LOGO: [&str; 4] = [
    "  _ __ _____      __| |_ ",
    " | '__/ _ \\ \\ /\\ / /| __|",
    " | | | (_) \\ V  V / | |_ ",
    " |_|  \\___/ \\_/\\_/   \\__|",
];

/// `present` = neutral screenshot state (matches the goldens: no focus
/// brightening, no selection highlight, no marquee offset).
pub fn draw(buf: &mut Buffer, area: Rect, app: &App, present: bool) -> Hit {
    let x0 = area.left();
    let y0 = area.top();
    let w = area.width;
    let h = area.height;
    if w < 8 || h < 12 {
        return Hit::default();
    }
    let border = theme::fg(theme::BORDER);

    // Single outer frame. The two panes and the server strip are NOT inset boxes;
    // they connect straight into this frame with ├ ┤ rules (┬ ┼ ┴ at the column
    // split), so the frame owns the only rounded corners. The "box" bounds ARE the
    // frame edges — all column/row math below is unchanged from the inset version,
    // only the border *drawing* differs.
    let xl = x0;
    let xr = x0 + w - 1;

    // Column split: conn width L · divider · err width R, over the full interior.
    let interior = (xr - 1) - (xl + 1) + 1; // content cells between the frame sides
    let err_w = split_err_width(interior);
    let div = xr - 1 - err_w; // center divider column

    // Vertical layout (rows, top→bottom). One blank row (y0+5) sits below the
    // identity band to give the logo breathing room; then the split divider.
    // Server health merges onto the closing ┴ rule with one breathing row above.
    let split_y = y0 + 6; // ├─┤ live · connections ├─┬─┤ errors & blocked ├─┤
    let hdr_y = split_y + 1; // summary/rate rows
    let cross_y = split_y + 5; // ├───┼───┤
    let col_y = split_y + 6; // column headers
    let list_y = split_y + 7; // data rows
    let bottom = y0 + h - 1; // frame bottom ╰──╯
    let chips_y = bottom - 1;
    let stats_y = bottom - 2;
    let merge_y = bottom - 3; // ├──┴─┤ server health ├─┤
    let breathe_y = bottom - 4; // blank split row (center │ kept)
    let list_bot = breathe_y - 1; // last data row
    let list_h = (list_bot as i32 - list_y as i32 + 1).max(0) as usize;

    let mut hit = Hit {
        side_by_side: true,
        ..Default::default()
    };

    // ---- frame top + identity band ----
    put(buf, xl, y0, "╭", border);
    hfill(buf, xl + 1, xr - 1, y0, '─', border);
    put(buf, xr, y0, "╮", border);
    for y in (y0 + 1)..=(y0 + 5) {
        put(buf, xl, y, "│", border);
        put(buf, xr, y, "│", border);
    }
    draw_identity(buf, x0, y0, xr, app, present, &mut hit);

    // ---- split divider (identity → tables), with both pane captions ----
    rule_row(buf, xl, xr, split_y, Some((div, "┬")), border);
    // The connections caption reflects the pane's view: "live connections", or
    // "connections · ▲ upload" / "· ▼ download" when flipped (`v`).
    let conn_cap = match app.conn_view.chip() {
        None => "live connections".to_string(),
        Some(c) => format!("connections · {c} · {}", app.band.label()),
    };
    draw_caption(buf, xl, split_y, &conn_cap, app, present, Focus::Conn, border);
    draw_caption(buf, div, split_y, "errors & blocked", app, present, Focus::Err, border);

    // ---- pane rows: sides + center │, with the header cross rule at cross_y ----
    for y in hdr_y..=breathe_y {
        if y == cross_y {
            rule_row(buf, xl, xr, y, Some((div, "┼")), border);
        } else {
            put(buf, xl, y, "│", border);
            put(buf, xr, y, "│", border);
            put(buf, div, y, "│", border);
        }
    }

    // left / right pane content
    let lx0 = xl + 1;
    let lw = div - lx0;
    draw_conn_pane(buf, lx0, lw, hdr_y, col_y, list_y, list_h, app, present, &mut hit);
    let rx0 = div + 1;
    let rw = (xr - 1) - rx0 + 1;
    draw_err_pane(buf, rx0, rw, hdr_y, col_y, list_y, list_h, app, present, &mut hit);

    // ---- server health: merge rule + full-width rows + frame bottom ----
    for y in [stats_y, chips_y] {
        put(buf, xl, y, "│", border);
        put(buf, xr, y, "│", border);
    }
    put(buf, xl, bottom, "╰", border);
    hfill(buf, xl + 1, xr - 1, bottom, '─', border);
    put(buf, xr, bottom, "╯", border);

    // Chips render at xl+2 with width (xr-xl-3); feed it back so App can
    // freeze/scroll the ring to keep the selection visible.
    hit.strip_w = (xr.saturating_sub(xl)).saturating_sub(3);
    draw_health(buf, xl, xr, div, merge_y, stats_y, chips_y, app, present, border, &mut hit);

    // App-level drag selection highlight (secondary copy path).
    if !present {
        if let Some(d) = app.drag {
            highlight(buf, Rect::new(d.lo(), d.row, d.hi().saturating_sub(d.lo()) + 1, 1));
        }
    }

    if app.help {
        draw_help(buf, area);
    }
    hit
}

/// Errors pane content width when side-by-side. Fits both goldens (interior
/// 144 -> 54, 206 -> 72) and keeps errors >= 1/3 of interior at the extremes.
fn split_err_width(interior: u16) -> u16 {
    let affine = ((interior as f32) * 0.29 + 12.1).round() as u16;
    let floor = interior.div_ceil(3); // ceil(interior/3)
    affine.max(floor).min(interior.saturating_sub(20))
}

fn draw_identity(buf: &mut Buffer, x0: u16, y0: u16, xr: u16, app: &App, present: bool, hit: &mut Hit) {
    let logo_st = theme::bold(theme::ESCAPE);
    for (i, line) in LOGO.iter().enumerate() {
        put(buf, x0 + 2, y0 + 1 + i as u16, line, logo_st);
    }
    let dimmer = theme::fg(theme::DIMMER);
    let bright = theme::fg(theme::BRIGHT);
    let id = &app.snap.identity;

    // Grid: left column labels @37 / values @47, right column labels @70 /
    // values @78. Rows 2–4 carry (mode·server) (sys proxy·router) (collector·
    // watch). `uptime` sits alone in the top-right corner (row 1), right-aligned
    // to the frame — see METRICS.md §5.

    // Row 1: MONITOR (left) · uptime (top-right corner)
    put(buf, x0 + 29, y0 + 1, "MONITOR", dimmer);
    {
        let val = id.uptime.as_str();
        let vx = (xr.saturating_sub(2)).saturating_sub(dw(val).saturating_sub(1)); // value hugs the corner
        let lx = vx.saturating_sub(8); // "uptime" (6) + 2-space gap
        put(buf, lx, y0 + 1, "uptime", dimmer);
        put(buf, vx, y0 + 1, val, bright);
    }

    // Row 2: status dot, in priority order:
    //   DOWN  (red)    — router/clash API unreachable
    //   PAUSED (gray)  — sampling frozen by the user
    //   ERROR (orange) — router up but the active server (or, in auto mode, the
    //                    whole pool) is failing its probe
    //   LIVE  (green)  — healthy; breathes to show sampling is live
    // Present/golden mode is always the neutral LIVE dot.
    let (dot_c, label, label_c, breathe) = if present {
        (theme::DIRECT, "LIVE", theme::BRIGHT, false)
    } else if !id.router_up {
        (theme::PERSISTENT, "DOWN", theme::PERSISTENT, false)
    } else if app.paused {
        (theme::DIM, "PAUSED", theme::DIM, false)
    } else if id.active_ok == Some(false) {
        (theme::UP, "ERROR", theme::UP, false)
    } else {
        (theme::DIRECT, "LIVE", theme::BRIGHT, true)
    };
    let dot_c = if breathe {
        theme::scale(dot_c, theme::pulse(app.started.elapsed().as_secs_f32()))
    } else {
        dot_c
    };
    put(buf, x0 + 29, y0 + 2, "●", theme::fg(dot_c));
    put(buf, x0 + 31, y0 + 2, label, theme::bold(label_c));
    put(buf, x0 + 37, y0 + 2, "mode", dimmer);
    // The left value column is at 47; the right column is pulled in to 62/70 (from
    // 70/78) to tighten the wide middle gap. Mode is bounded to the 15-cell left
    // value column so it can't run into the right label.
    put(buf, x0 + 47, y0 + 2, &truncate(&id.mode, 15), bright);

    let rl = x0 + 62; // right column: label
    let rv = x0 + 70; //               value
    // Row 2 right: server — moved up into the top row of the right column.
    put(buf, rl, y0 + 2, "server", dimmer);
    // Reserve name width from the pool's longest name so the ms column is stable;
    // bounded (<=15). 8 reproduces the golden (JP-Tokyo).
    let reserve = id.name_reserve.clamp(6, 15);
    let name_st = if present || id.active_ok == Some(true) {
        theme::bold(theme::ESCAPE)
    } else {
        theme::fg(theme::DIM)
    };
    put(buf, rv, y0 + 2, &truncate(&id.server_name, reserve), name_st);
    let (ms, ms_st) = match id.server_ms {
        Some(v) => (format!("{} ms", v), theme::bold(theme::latency_color(v))),
        None => ("—".to_string(), theme::fg(theme::DIM)),
    };
    put(buf, rv + 1 + reserve, y0 + 2, &ms, ms_st);

    // Row 3: sys proxy (clickable) · router
    // "sys proxy <state>" toggles the proxy on click — expose the whole
    // label+value span and highlight it while the mouse hovers over it.
    let proxy = app.proxy_display();
    let px = x0 + 37;
    let pw = (47 - 37) + dw(&proxy); // label col .. end of value
    let proxy_rect = Rect::new(px, y0 + 3, pw, 1);
    hit.sysproxy = proxy_rect;
    let hovered = !present && app.hover.is_some_and(|(cx, cy)| rect_has(proxy_rect, cx, cy));
    let base = match proxy.as_str() {
        "on" => theme::DIRECT,
        "off" => theme::PERSISTENT,
        _ => theme::UP,
    };
    let (label_st, proxy_st) = if hovered {
        let u = |c: Color| Style::default().fg(theme::brighten(c, 0.25)).add_modifier(Modifier::UNDERLINED);
        (u(theme::DIMMER), u(base))
    } else {
        (dimmer, theme::fg(base))
    };
    put(buf, x0 + 37, y0 + 3, "sys proxy", label_st);
    put(buf, x0 + 47, y0 + 3, &proxy, proxy_st);

    // Row 3 right: "running · N%" (CPU colored — orange/red flags a spin/wedge)
    // or, when down, "down · <reason>" in red so a glance says what to fix.
    put(buf, rl, y0 + 3, "router", dimmer);
    if id.router_up {
        put(buf, rv, y0 + 3, &id.router, bright);
        if let Some(cpu) = id.router_cpu {
            let cpu_c = if cpu >= 120.0 {
                theme::PERSISTENT
            } else if cpu >= 50.0 {
                theme::UP
            } else {
                theme::DIRECT
            };
            put(buf, rv + dw(&id.router) + 1, y0 + 3, "·", dimmer);
            put(buf, rv + dw(&id.router) + 3, y0 + 3, &format!("{cpu:.0}%"), theme::fg(cpu_c));
        }
    } else {
        let down = if id.router_reason.is_empty() {
            "down".to_string()
        } else {
            format!("down · {}", id.router_reason)
        };
        put(buf, rv, y0 + 3, &down, theme::fg(theme::PERSISTENT));
    }

    // Row 4: collector · watch — both 9-char labels, so the left one aligns under
    // "sys proxy". Same on/off/— color coding as watch.
    put(buf, x0 + 37, y0 + 4, "collector", dimmer);
    put(buf, x0 + 47, y0 + 4, &id.collector, status_color(&id.collector));
    put(buf, rl, y0 + 4, "watch", dimmer);
    put(buf, rv, y0 + 4, &id.watch, status_color(&id.watch));
}

/// Shared on/off/— coloring for the watch + collector status values: green =
/// active, orange = installed-but-stopped, dim = absent.
fn status_color(v: &str) -> Style {
    match v {
        "on" => theme::fg(theme::DIRECT),
        "off" => theme::fg(theme::UP),
        _ => theme::fg(theme::DIM),
    }
}

/// A horizontal rule that connects into the frame: `├` at `xl`, `─` fill, `┤` at
/// `xr`, and an optional junction glyph at a divider column (`┬`/`┼`/`┴`). Section
/// captions (`─┤ label ├`) are overlaid afterwards by `draw_caption`.
fn rule_row(buf: &mut Buffer, xl: u16, xr: u16, y: u16, junction: Option<(u16, &str)>, border: Style) {
    put(buf, xl, y, "├", border);
    hfill(buf, xl + 1, xr - 1, y, '─', border);
    put(buf, xr, y, "┤", border);
    if let Some((d, glyph)) = junction {
        put(buf, d, y, glyph, border);
    }
}

/// Draw `─┤ caption ├` starting just after the corner/divider at `corner`, in
/// focus-aware styling.
#[allow(clippy::too_many_arguments)]
fn draw_caption(buf: &mut Buffer, corner: u16, y: u16, label: &str, app: &App, present: bool, which: Focus, border: Style) {
    // The ┤ ├ connectors always match the border; focus is shown by brightening
    // the caption text only, so the connectors never look out of step.
    let focused = !present && app.focus == which;
    // A locked/frozen selection tints the caption amber (§5.2) — the single split
    // box can't carry a per-pane border ring, so focus + lock show in the caption.
    let locked = focused
        && match which {
            Focus::Conn => app.conn_active(),
            Focus::Err => app.err_active(),
            _ => false,
        };
    let cstyle = if locked {
        theme::bold(theme::ARMED)
    } else if focused {
        theme::bold(theme::BRIGHT)
    } else {
        theme::fg(theme::DIMMER)
    };
    let mut x = corner + 1;
    put(buf, x, y, "─┤ ", border);
    x += 3;
    put(buf, x, y, label, cstyle);
    x += dw(label);
    // Active lane filter chip — shown on both panes, since the filter now scopes
    // the connections list AND the errors pane.
    let _ = which;
    if let Some(l) = app.lane_filter {
        let chip = format!(" · {}", l.label());
        put(buf, x, y, &chip, theme::fg(l.color()));
        x += dw(&chip);
    }
    put(buf, x, y, " ├", border);
}

// ---------------- connections pane ----------------

#[allow(clippy::too_many_arguments)]
fn draw_conn_pane(
    buf: &mut Buffer,
    x0: u16,
    w: u16,
    hdr_y: u16,
    col_y: u16,
    list_y: u16,
    list_h: usize,
    app: &App,
    present: bool,
    hit: &mut Hit,
) {
    let dimmer = theme::fg(theme::DIMMER);
    let dim = theme::fg(theme::DIM);
    let up = theme::fg(theme::UP);
    let down = theme::fg(theme::DOWN);

    // Header rows = a per-lane aggregate (all + escape/corp/direct) of the detail
    // rows below, matching whichever columns the current view shows: Live sums
    // #conns and ↑/↓ byte totals (aligned under the #/UP/DOWN columns) alongside
    // the live ↑/↓ rate; a flipped view sums the four band columns (aligned under
    // them). Block is excluded, as in the table.
    #[derive(Default, Clone, Copy)]
    struct Agg {
        conns: u32,
        lup: f64,
        ldn: f64,
        hu: [u64; 4],
        hd: [u64; 4],
    }
    let acc = |a: &mut Agg, r: &crate::model::ConnRow| {
        a.conns += r.conns;
        a.lup += r.live_up;
        a.ldn += r.live_down;
        for i in 0..4 {
            a.hu[i] += r.hist_up[i];
            a.hd[i] += r.hist_down[i];
        }
    };
    let (mut all, mut esc, mut corp, mut direct) = (Agg::default(), Agg::default(), Agg::default(), Agg::default());
    for r in &app.rows {
        match r.lane {
            Lane::Escape => acc(&mut esc, r),
            Lane::Corp => acc(&mut corp, r),
            Lane::Direct => acc(&mut direct, r),
            Lane::Block => continue,
        }
        acc(&mut all, r);
    }
    let header = [(None, all), (Some(Lane::Escape), esc), (Some(Lane::Corp), corp), (Some(Lane::Direct), direct)];

    let is_live = app.conn_view.is_live();
    let dir_up = app.conn_view.is_up();
    let bcols = app.band.cols();
    const HCOLW: u16 = 8;
    let hcol_x = |k: u16| x0 + w - 2 - k * HCOLW; // aligns with the metrics data columns
    let hfmt = |v: u64, span: i64, is_rate: bool| -> String {
        if is_rate {
            format!("{}/s", format::bytes_total(v as f64 / span.max(1) as f64))
        } else {
            format::bytes_total(v as f64)
        }
    };
    for (i, (lane, agg)) in header.iter().enumerate() {
        let y = hdr_y + i as u16;
        if y >= col_y {
            break;
        }
        let strong = lane.is_none();
        let name_st = match lane {
            None => theme::bold(theme::BRIGHT),
            Some(l) => theme::bold(l.color()),
        };
        put(buf, x0 + 1, y, lane.map_or("all", |l| l.label()), name_st);
        if is_live {
            // Live ↑/↓ rate on the left (from the snapshot) — only when the pane is
            // wide enough not to collide with the aligned # column.
            let (ru, rd) = match lane {
                None => (app.snap.all.up, app.snap.all.down),
                Some(l) => app.snap.lanes.iter().find(|a| a.lane == *l).map(|a| (a.up, a.down)).unwrap_or((0.0, 0.0)),
            };
            let idle = agg.conns == 0;
            let rate_st = if strong { theme::bold(theme::BRIGHT) } else { dim };
            if w >= 72 {
                put(buf, x0 + 8, y, "↑", up);
                if idle {
                    put(buf, x0 + 10, y, "—", dim);
                } else {
                    let (v, u2) = format::rate_parts(ru);
                    put(buf, x0 + 10, y, &format!("{v} {u2}"), rate_st);
                }
                put(buf, x0 + 21, y, "↓", down);
                if idle {
                    put(buf, x0 + 23, y, "—", dim);
                } else {
                    let (v, u2) = format::rate_parts(rd);
                    put(buf, x0 + 23, y, &format!("{v} {u2}"), rate_st);
                }
            }
            // #conns / ↑bytes / ↓bytes aligned under the data columns.
            put_right(buf, x0 + w - 38, y, &agg.conns.to_string(), if strong { theme::bold(theme::BRIGHT) } else { dimmer });
            let (ust, dst) = if strong {
                (theme::bold(theme::BRIGHT), theme::bold(theme::BRIGHT))
            } else {
                (theme::fg(theme::UP_TABLE), theme::fg(theme::DOWN_TABLE))
            };
            put_right(buf, x0 + w - 27, y, &format::bytes_total(agg.lup), ust);
            put_right(buf, x0 + w - 17, y, &format::bytes_total(agg.ldn), dst);
        } else {
            let series = if dir_up { &agg.hu } else { &agg.hd };
            let dir_c = if dir_up { theme::UP_TABLE } else { theme::DOWN_TABLE };
            for k in 0..4u16 {
                let (_, span, is_rate) = bcols[(3 - k) as usize];
                let st = if strong { theme::bold(theme::BRIGHT) } else { theme::fg(dir_c) };
                put_right(buf, hcol_x(k), y, &hfmt(series[(3 - k) as usize], span, is_rate), st);
            }
        }
        hit.lanes.push((Rect::new(x0 + 1, y, w - 2, 1), *lane));
    }

    // Detail list: the lane-filtered subset (the header above stays all-lanes).
    // The columns pan with the view; host:port pinned; dormant rows greyed.
    let visible = app.visible_rows();
    let scroll = app.conn_scroll.min(visible.len().saturating_sub(1));
    hit.conn_list = Rect::new(x0, list_y, w, list_h as u16);
    hit.conn_pane = Rect::new(x0, hdr_y, w, (list_y + list_h as u16).saturating_sub(hdr_y));
    hit.conn_h = list_h;
    if visible.is_empty() {
        draw_empty_match(buf, x0, list_y, w, app);
    }
    if app.conn_view.is_live() {
        draw_conn_live_cols(buf, x0, w, col_y, list_y, list_h, scroll, &visible, app, present);
    } else {
        draw_conn_metric_cols(buf, x0, w, col_y, list_y, list_h, scroll, &visible, app, present);
    }
    if !present {
        draw_scrollbar(buf, x0 + w - 1, list_y, list_h, visible.len(), scroll);
    }
}

/// The connections column headers ARE the direction indicator in the flipped
/// views (`↑`/`↓` on each label); Live keeps its `# / UP / DOWN / RULE`.
/// Both render the shared `app.rows`.
#[allow(clippy::too_many_arguments)]
fn draw_conn_live_cols(buf: &mut Buffer, x0: u16, w: u16, col_y: u16, list_y: u16, list_h: usize, scroll: usize, rows: &[&crate::model::ConnRow], app: &App, present: bool) {
    let dimmer = theme::fg(theme::DIMMER);
    let dim = theme::fg(theme::DIM);
    put(buf, x0 + 1, col_y, "LANE", dimmer);
    put(buf, x0 + 8, col_y, "HOST:PORT", dimmer);
    put_right(buf, x0 + w - 38, col_y, "#", dimmer);
    put_right(buf, x0 + w - 27, col_y, "UP", dimmer);
    put_right(buf, x0 + w - 17, col_y, "DOWN", dimmer);
    put(buf, x0 + w - 14, col_y, "RULE", dimmer);
    let host_max = (w.saturating_sub(38)).saturating_sub(9);
    for row in 0..list_h {
        let idx = scroll + row;
        if idx >= rows.len() {
            break;
        }
        let c = rows[idx];
        let y = list_y + row as u16;
        let dormant = !c.is_live();
        put(buf, x0 + 1, y, c.lane.label(), if dormant { dim } else { theme::bold(c.lane.color()) });
        let hostport = host_label(c);
        let selected = !present && app.focus == Focus::Conn && app.conn_active() && idx == app.conn_sel;
        let shown = if selected { marquee(&hostport, host_max, app.started.elapsed().as_secs_f32()) } else { truncate(&hostport, host_max) };
        put(buf, x0 + 8, y, &shown, if dormant { dim } else { theme::fg(theme::BRIGHT) });
        put_right(buf, x0 + w - 38, y, &c.conns.to_string(), dimmer);
        let (up_st, down_st) = if dormant { (dim, dim) } else { (theme::fg(theme::UP_TABLE), theme::fg(theme::DOWN_TABLE)) };
        put_right(buf, x0 + w - 27, y, &format::bytes_total(c.live_up), up_st);
        put_right(buf, x0 + w - 17, y, &format::bytes_total(c.live_down), down_st);
        put(buf, x0 + w - 14, y, &truncate(&c.rule, 13), dimmer);
        if selected {
            highlight_row(buf, x0, y, w, c.lane.color());
        }
    }
}

/// The flipped view: per-domain byte history over the band's four trailing-window
/// columns in the current direction (`↑`/`↓` prefix the column labels). Dormant
/// hosts (not currently connected) render greyed. See METRICS.md §5.
#[allow(clippy::too_many_arguments)]
fn draw_conn_metric_cols(buf: &mut Buffer, x0: u16, w: u16, col_y: u16, list_y: u16, list_h: usize, scroll: usize, rows: &[&crate::model::ConnRow], app: &App, present: bool) {
    let dimmer = theme::fg(theme::DIMMER);
    let dim = theme::fg(theme::DIM);
    let up = app.conn_view.is_up();
    let arrow = if up { "↑" } else { "↓" };
    let dir_c = if up { theme::UP_TABLE } else { theme::DOWN_TABLE };
    let cols = app.band.cols(); // [(label, span_secs, is_rate); 4], index 0 = leftmost
    const COLW: u16 = 8;
    let col_x = |k: u16| x0 + w - 2 - k * COLW; // k=0 rightmost = band col index 3
    let host_max = w.saturating_sub(4 * COLW + 10);
    let fmt = |v: u64, span: i64, is_rate: bool| -> String {
        if is_rate { format!("{}/s", format::bytes_total(v as f64 / span.max(1) as f64)) } else { format::bytes_total(v as f64) }
    };

    // Column header: LANE HOST + arrow-prefixed window labels (the arrow is the
    // up/down indicator, per the "arrows in the column labels" request).
    put(buf, x0 + 1, col_y, "LANE", dimmer);
    put(buf, x0 + 8, col_y, "HOST", dimmer);
    for k in 0..4u16 {
        let (lbl, _, _) = cols[(3 - k) as usize];
        put_right(buf, col_x(k), col_y, &format!("{arrow}{lbl}"), theme::fg(dir_c));
    }

    for row in 0..list_h {
        let idx = scroll + row;
        if idx >= rows.len() {
            break;
        }
        let c = rows[idx];
        let y = list_y + row as u16;
        let dormant = !c.is_live();
        put(buf, x0 + 1, y, c.lane.label(), if dormant { dim } else { theme::bold(c.lane.color()) });
        let selected = !present && app.focus == Focus::Conn && app.conn_active() && idx == app.conn_sel;
        let host = host_label(c);
        let shown = if selected { marquee(&host, host_max, app.started.elapsed().as_secs_f32()) } else { truncate(&host, host_max) };
        put(buf, x0 + 8, y, &shown, if dormant { dim } else { theme::fg(theme::BRIGHT) });
        let series = if up { &c.hist_up } else { &c.hist_down };
        for k in 0..4u16 {
            let (_, span, is_rate) = cols[(3 - k) as usize];
            let v = series[(3 - k) as usize];
            let st = if dormant { dim } else if v > 0 { theme::fg(dir_c) } else { dim };
            put_right(buf, col_x(k), y, &fmt(v, span, is_rate), st);
        }
        if selected {
            highlight_row(buf, x0, y, w, c.lane.color());
        }
    }
}

/// When a pane's detail list is emptied by an active search, say so rather than
/// leaving a blank pane (only when a search is actually filtering — a genuinely
/// empty pane with no search stays blank, as before).
fn draw_empty_match(buf: &mut Buffer, x0: u16, list_y: u16, w: u16, app: &App) {
    if let Some(pat) = app.search_pattern() {
        let msg = truncate(&format!("no hosts match /{pat}/"), w.saturating_sub(2));
        put(buf, x0 + 1, list_y, &msg, theme::fg(theme::DIM));
    }
}

/// A row's pinned first-column label: `host:port` for a live connection, or just
/// `host` for a historical row (no live port).
fn host_label(c: &crate::model::ConnRow) -> String {
    if c.port == 0 {
        c.host.clone()
    } else {
        format!("{}:{}", c.host, c.port)
    }
}

// ---------------- errors pane ----------------

#[allow(clippy::too_many_arguments)]
fn draw_err_pane(
    buf: &mut Buffer,
    x0: u16,
    w: u16,
    hdr_y: u16,
    col_y: u16,
    list_y: u16,
    list_h: usize,
    app: &App,
    present: bool,
    hit: &mut Hit,
) {
    let dimmer = theme::fg(theme::DIMMER);
    let dim = theme::fg(theme::DIM);

    // Header row 0: "aggregated over" + window tabs.
    put(buf, x0 + 1, hdr_y, "aggregated over", dimmer);
    draw_windows(buf, x0, w, hdr_y, app, hit);

    // Category rows.
    let cats = [
        ("transient", theme::TRANSIENT, app.snap.transient),
        ("persistent", theme::PERSISTENT, app.snap.persistent),
        ("blocked", theme::BLOCKED, app.snap.blocked),
    ];
    for (i, (label, color, cat)) in cats.iter().enumerate() {
        let y = hdr_y + 1 + i as u16;
        if y >= col_y {
            break;
        }
        put(buf, x0 + 1, y, label, theme::bold(*color));
        put_right(buf, x0 + 15, y, &cat.count.to_string(), theme::bold(theme::BRIGHT));
        put(buf, x0 + 16, y, &fmt_dom(cat), dimmer);
    }

    // Column header.
    put_right(buf, x0 + 5, col_y, "COUNT", dimmer);
    put(buf, x0 + 9, col_y, "TYPE", dimmer);
    put(buf, x0 + 21, col_y, "DOMAIN", dimmer);

    // Data rows (search-filtered; the category header above stays whole).
    let errs = app.errors_view();
    let scroll = app.err_scroll.min(errs.len().saturating_sub(1));
    hit.err_list = Rect::new(x0, list_y, w, list_h as u16);
    hit.err_pane = Rect::new(x0, hdr_y, w, (list_y + list_h as u16).saturating_sub(hdr_y));
    hit.err_h = list_h;
    if errs.is_empty() {
        draw_empty_match(buf, x0, list_y, w, app);
    }
    let dom_max = w.saturating_sub(21);
    for row in 0..list_h {
        let idx = scroll + row;
        if idx >= errs.len() {
            break;
        }
        let e = &errs[idx];
        let y = list_y + row as u16;
        put_right(buf, x0 + 5, y, &e.count.to_string(), theme::bold(theme::BRIGHT));
        // TYPE carries the category by color: dns=transient orange,
        // timeout/reset/refused=persistent red, blocked=purple.
        put(buf, x0 + 9, y, e.kind.label(), theme::fg(e.kind.color()));
        let selected = !present && app.focus == Focus::Err && app.err_active() && idx == app.err_sel;
        let shown = if selected {
            marquee(&e.domain, dom_max, app.started.elapsed().as_secs_f32())
        } else {
            truncate(&e.domain, dom_max)
        };
        put(buf, x0 + 21, y, &shown, dim);
        if selected {
            highlight_row(buf, x0, y, w, e.kind.color());
        }
    }
    if !present {
        draw_scrollbar(buf, x0 + w - 1, list_y, list_h, errs.len(), scroll);
    }
}

fn fmt_dom(cat: &ErrCat) -> String {
    format!(" · {} dom", cat.domains)
}

fn draw_windows(buf: &mut Buffer, x0: u16, w: u16, y: u16, app: &App, hit: &mut Hit) {
    // Build the "5m [10m] 1h 24h" string and remember each tab's cell span.
    let mut parts: Vec<(String, Window, bool)> = Vec::new();
    for win in Window::ALL {
        let sel = win == app.window;
        let s = if sel {
            format!("[{}]", win.label())
        } else {
            win.label().to_string()
        };
        parts.push((s, win, sel));
    }
    let total: u16 = parts.iter().map(|(s, _, _)| dw(s)).sum::<u16>() + (parts.len() as u16 - 1);
    let mut x = (x0 + w - 2).saturating_sub(total.saturating_sub(1));
    let dimmer = theme::fg(theme::DIMMER);
    for (i, (s, win, sel)) in parts.iter().enumerate() {
        if i > 0 {
            x += 1; // separating space
        }
        let st = if *sel { theme::bold(theme::BRIGHT) } else { dimmer };
        put(buf, x, y, s, st);
        hit.windows.push((Rect::new(x, y, dw(s), 1), *win));
        x += dw(s);
    }
}

// ---------------- server health ----------------

/// Server health connects onto the closing `┴` merge rule (no inset box): the
/// `merge_y` rule carries the `server health` tab, then full-width stats + chips
/// rows. `div` is the column split above, closed here by `┴`.
#[allow(clippy::too_many_arguments)]
fn draw_health(
    buf: &mut Buffer,
    xl: u16,
    xr: u16,
    div: u16,
    merge_y: u16,
    stats_y: u16,
    chips_y: u16,
    app: &App,
    present: bool,
    border: Style,
    hit: &mut Hit,
) {
    // Merge divider: the two panes close into ┴ here, and the section tab rides
    // the same rule (right of the ┴).
    rule_row(buf, xl, xr, merge_y, Some((div, "┴")), border);
    // Caption gains a focus ring like the panes (§5.1): brighten when focused,
    // amber once a chip is selected (frozen strip).
    let focused = !present && app.focus == Focus::Health;
    let cap = if focused && app.strip_sel.is_some() {
        theme::bold(theme::ARMED)
    } else if focused {
        theme::bold(theme::BRIGHT)
    } else {
        theme::fg(theme::DIMMER)
    };
    put(buf, div + 1, merge_y, "─┤ ", border);
    put(buf, div + 4, merge_y, "server health", cap);
    put(buf, div + 4 + dw("server health"), merge_y, " ├", border);

    let x0 = xl + 1;
    let w = (xr - 1) - x0 + 1;
    let s = &app.snap;
    // Stats: the active server is named in the header identity band and marked
    // in the strip below, so it is not repeated here.
    let stats = format!("{} servers · {} up · {} down", s.servers_total, s.servers_up, s.servers_down);
    put(buf, x0 + 1, stats_y, &stats, theme::fg(theme::DIM));

    // chips row — or a "probing…" hint while the first round is still running
    // (router up, pool known, but nothing has come back yet), so an empty strip
    // never looks broken.
    if !present && s.identity.router_up && s.servers_total > 0 && s.servers_up == 0 && s.servers_down == 0 {
        put(buf, x0 + 1, chips_y, "probing…", theme::fg(theme::DIM));
    } else {
        draw_chips(buf, x0 + 1, chips_y, w.saturating_sub(2), app, present, hit);
    }
}

/// Draw the server strip. When a chip is selected the marquee is frozen: the ring
/// is rendered at `app.strip_off` (the exact cell offset it had when frozen, so it
/// doesn't jump — a partial chip may sit at the left edge) and scrolls only to
/// keep the selection visible (`App::reveal_strip`); otherwise it marquees.
fn draw_chips(buf: &mut Buffer, x0: u16, y: u16, w: u16, app: &App, present: bool, hit: &mut Hit) {
    let bright = theme::fg(theme::BRIGHT);
    let escape = theme::fg(theme::ESCAPE);
    let sel = if !present && app.focus == Focus::Health { app.strip_sel } else { None };
    // Each chip is a run of styled segments. The active server leads with a ▶;
    // the *selected* chip (frozen strip) is tinted amber + selection background.
    let chips: Vec<Vec<(String, Style)>> = app
        .snap
        .chips
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let picked = sel == Some(i);
            let bg = |st: Style| if picked { st.bg(theme::SELECTION_BG) } else { st };
            let lat = bg(theme::fg(theme::latency_color(c.ms)));
            let name_st = if picked {
                bg(theme::bold(theme::ARMED))
            } else if c.active {
                theme::bold(theme::ESCAPE)
            } else {
                bright
            };
            let ms = format!("{:>3} ms", c.ms);
            if c.active {
                vec![("▶ ".to_string(), bg(escape)), (c.name.clone(), name_st), (" ".to_string(), bg(bright)), (ms, lat)]
            } else {
                vec![(c.name.clone(), name_st), (" ".to_string(), bg(bright)), (ms, lat)]
            }
        })
        .collect();
    if chips.is_empty() {
        return;
    }
    let widths: Vec<u16> = chips.iter().map(|segs| segs.iter().map(|(s, _)| dw(s)).sum()).collect();
    let total: u16 = widths.iter().sum::<u16>() + 3 * (chips.len().saturating_sub(1) as u16);

    // Whole pool fits (static left-to-right layout, whether or not selected).
    if present || total <= w {
        hit.strip_render_off = 0; // static: everything starts at column 0
        let mut col = x0;
        for (i, segs) in chips.iter().enumerate() {
            let sep = if i > 0 { 3 } else { 0 };
            if col + sep + widths[i] > x0 + w {
                break;
            }
            col += sep;
            hit.chips.push((Rect::new(col, y, widths[i], 1), i));
            for (s, st) in segs {
                put(buf, col, y, s, *st);
                col += dw(s);
            }
        }
        hover_chip(buf, app, present, hit);
        return;
    }

    // Ring: one cell buffer (chips + 3-cell separators), rendered at a cell offset.
    // Frozen at `app.strip_off` when a chip is selected, else the time-based marquee.
    let mut cells: Vec<(char, Style)> = Vec::new();
    let mut starts: Vec<usize> = Vec::with_capacity(chips.len());
    for (i, segs) in chips.iter().enumerate() {
        if i > 0 {
            cells.extend([(' ', Style::default()); 3]);
        }
        starts.push(cells.len());
        for (s, st) in segs {
            cells.extend(s.chars().map(|c| (c, *st)));
        }
    }
    let span = cells.len() + 3;
    let off = match sel {
        Some(_) => app.strip_off % span,
        // Marquee off a resettable baseline (not raw elapsed) so it resumes from
        // the frozen offset after unfreezing instead of jumping.
        None => (app.marquee_off0 + (app.marquee_t0.elapsed().as_secs_f32() * MARQUEE_CPS) as usize) % span,
    };
    hit.strip_render_off = off; // freezing captures exactly this, so the view can't jump
    for k in 0..w as usize {
        let ci = (off + k) % span;
        let (ch, st) = if ci < cells.len() { cells[ci] } else { (' ', Style::default()) };
        let mut b = [0u8; 4];
        put(buf, x0 + k as u16, y, ch.encode_utf8(&mut b), st);
    }
    // Record each on-screen chip's rect so a click can select it — including the
    // two edge chips that are only *partially* visible: the last one (clipped on
    // the right) and the first one (clipped on the left, its left edge scrolled
    // off, so its right portion shows from column 0).
    let wu = w as usize;
    for (i, &st) in starts.iter().enumerate() {
        let wd = widths[i] as usize;
        let kl = (st + span - off % span) % span; // display col of the chip's left edge
        let (cx, cw) = if kl < wu {
            (kl, wd.min(wu - kl)) // left edge on screen (may be right-clipped)
        } else if st < off && off < st + wd {
            (0, (st + wd - off).min(wu)) // left-clipped: right part shows from col 0
        } else {
            continue;
        };
        if cw > 0 {
            hit.chips.push((Rect::new(x0 + cx as u16, y, cw as u16, 1), i));
        }
    }
    hover_chip(buf, app, present, hit);
}

/// Hover feedback for the server strip: brighten + underline the chip under the
/// pointer (mirrors the `sys proxy` hover). A post-pass over `hit.chips`, so it
/// works for both the static and the frozen-ring layouts.
fn hover_chip(buf: &mut Buffer, app: &App, present: bool, hit: &Hit) {
    if present {
        return;
    }
    let Some((hx, hy)) = app.hover else { return };
    if let Some((r, _)) = hit.chips.iter().find(|(r, _)| rect_has(*r, hx, hy)) {
        for x in r.left()..r.right() {
            if let Some(c) = buf.cell_mut((x, r.top())) {
                let f = c.fg;
                c.set_fg(theme::brighten(f, 0.25));
                c.modifier.insert(Modifier::UNDERLINED);
            }
        }
    }
}

// ---------------- helpers ----------------

/// Whether cell (col,row) is inside a non-empty rect.
fn rect_has(r: Rect, col: u16, row: u16) -> bool {
    r.width > 0 && r.height > 0 && col >= r.left() && col < r.right() && row >= r.top() && row < r.bottom()
}

/// Selected-row style: a subtle consistent background, brightened text, and a
/// thin accent bar at the front in the row's semantic color.
fn highlight_row(buf: &mut Buffer, x0: u16, y: u16, w: u16, accent: Color) {
    for x in x0..x0 + w {
        if let Some(c) = buf.cell_mut((x, y)) {
            c.set_bg(theme::SELECTION_BG);
            let f = c.fg;
            c.set_fg(theme::brighten(f, 0.30));
        }
    }
    if let Some(c) = buf.cell_mut((x0, y)) {
        c.set_symbol("▎");
        c.set_fg(accent);
        c.set_bg(theme::SELECTION_BG);
    }
}

fn highlight(buf: &mut Buffer, rect: Rect) {
    for x in rect.left()..rect.right() {
        if let Some(c) = buf.cell_mut((x, rect.top())) {
            c.set_style(Style::default().add_modifier(Modifier::REVERSED));
        }
    }
}

fn draw_scrollbar(buf: &mut Buffer, x: u16, y0: u16, h: usize, total: usize, scroll: usize) {
    if total <= h || h == 0 {
        return;
    }
    // thin position tick (btop-style) on the right border column
    let track = h;
    let pos = (scroll * (track.saturating_sub(1))) / total.saturating_sub(1).max(1);
    let y = y0 + pos.min(track - 1) as u16;
    put(buf, x, y, "▐", theme::fg(theme::DIM));
}

/// Cells-per-second for horizontal auto-scroll (time-based, so the speed is
/// steady regardless of redraw/event cadence). The frozen offset is fed back to
/// `App` via `Hit::strip_render_off`, so `App` never recomputes this itself.
const MARQUEE_CPS: f32 = 5.0;

/// Horizontal auto-scroll of an overflowing value (selected row only). `secs` is
/// elapsed wall-clock time so the roll speed doesn't change with event rate.
fn marquee(s: &str, max: u16, secs: f32) -> String {
    let width = dw(s);
    if width <= max {
        return s.to_string();
    }
    let pad = 3u16;
    let span = width + pad; // s + gap
    let off = ((secs * MARQUEE_CPS) as u64 % span as u64) as u16;
    let chars: Vec<char> = s.chars().chain(std::iter::repeat_n(' ', pad as usize)).collect();
    let mut out = String::new();
    for k in 0..max {
        let ci = ((off + k) % span) as usize;
        out.push(chars[ci]);
    }
    out
}

fn draw_help(buf: &mut Buffer, area: Rect) {
    let lines = [
        "  rowt monitor — keys",
        "",
        "  ↑↓ / j k   move selection (locks the row)",
        "  ←→ / h l   switch pane · select server chip",
        "  Tab        cycle focus (conns/errors/health)",
        "  v          flip pane · live / ↑ upload / ↓ download",
        "  s          span — metrics band (recent/days/year); from Live, opens ↑ upload",
        "  f 1 2 3 0  lane filter / jump / clear",
        "  /          search hosts (regex) · esc clears",
        "  w [ ]      errors window (rolling)",
        "  y          copy the selected domain",
        "  e c b d    route selected → escape/corp/",
        "             block / direct  (↵ or key×2 apply)",
        "  u          use the selected server",
        "  o          toggle the system proxy on/off",
        "  r          re-probe servers now",
        "  p          pause sampling   · esc  cancel",
        "  ?          toggle this help · q  quit",
        "",
        "  observe + confirmed, reversible overrides.",
    ];
    // Pad every line to the widest so the box is opaque regardless of content.
    let cw = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let bw = cw + 1; // one trailing pad column
    let bh = lines.len() as u16 + 2;
    let bx = area.left() + (area.width.saturating_sub(bw + 2)) / 2;
    let by = area.top() + (area.height.saturating_sub(bh)) / 2;
    let border = theme::fg(theme::BORDER_FOCUS);
    put(buf, bx, by, "╭", border);
    hfill(buf, bx + 1, bx + bw, by, '─', border);
    put(buf, bx + bw + 1, by, "╮", border);
    for (i, l) in lines.iter().enumerate() {
        let y = by + 1 + i as u16;
        put(buf, bx, y, "│", border);
        hfill(buf, bx + 1, bx + bw, y, ' ', theme::fg(theme::BRIGHT)); // opaque row
        put(buf, bx + 1, y, l, theme::fg(theme::BRIGHT));
        put(buf, bx + bw + 1, y, "│", border);
    }
    let yb = by + bh - 1;
    put(buf, bx, yb, "╰", border);
    hfill(buf, bx + 1, bx + bw, yb, '─', border);
    put(buf, bx + bw + 1, yb, "╯", border);
}

/// The bottom hint bar (interactive only; not part of the golden frame). Two
/// states (CONTROLS.md §6): the amber confirm bar when an edit is armed, else a
/// global key group plus a contextual group for the current selection/strip.
pub fn draw_footer(buf: &mut Buffer, area: Rect, app: &App) {
    let dimmer = theme::fg(theme::DIMMER);
    let y = area.bottom().saturating_sub(1);
    let left = area.left();

    // Search editor owns the footer while open (a little line editor with a block
    // cursor); everything else is suppressed until Enter/Esc.
    if app.search.editing {
        draw_search_editor(buf, area, y, app);
        return;
    }

    // Armed → confirm bar (overrides the whole left side).
    if let Some(a) = &app.armed {
        let bar = format!(" CONFIRM  {}  · press {} again or ↵ to apply · esc cancel ", a.label(), a.key);
        let shown = truncate(&bar, area.width);
        put(buf, left, y, &shown, theme::bold(theme::ARMED));
        return;
    }

    // Normal: global group, then a contextual group when something is live.
    let global = if app.paused {
        " ↑↓←→ navigate · v flip · s span · f lane · / search · w window · o proxy · p resume · ? help · q quit "
    } else {
        " ↑↓←→ navigate · v flip · s span · f lane · / search · w window · o proxy · p pause · ? help · q quit "
    };
    let shown = truncate(global, area.width);
    put(buf, left, y, &shown, dimmer);
    let mut x = left + dw(&shown);

    // Contextual group: same colour as the global keys, set off by a vertical bar
    // — and the bar only appears when there actually are contextual keys. (`w`/`s`
    // are global now, so they live in the global group, not here.)
    let ctx: Option<String> = match app.focus {
        Focus::Conn if app.conn_active() => Some("e·c·b·d route · y copy ".to_string()),
        Focus::Err if app.err_active() => Some("e·c·b·d route · y copy ".to_string()),
        Focus::Health => match app.strip_sel.and_then(|i| app.snap.chips.get(i)) {
            None => Some("←→ select server ".to_string()),
            Some(s) if !s.active => Some(format!("u use {} ", s.name)),
            Some(_) => Some("active server ".to_string()),
        },
        _ => None,
    };
    if let Some(c) = ctx {
        let group = format!("│ {c}");
        if x + 1 < area.right() {
            let group = truncate(&group, area.right().saturating_sub(x));
            put(buf, x, y, &group, dimmer);
            x += dw(&group);
        }
    }

    // Right edge, in priority order: a pending-reload countdown chip, else a
    // transient toast, else — when neither is showing — the persistent search
    // indicator, which degrades as it would collide with the hints (`x` = the
    // first free column past them).
    let toast_fresh = app.toast.as_ref().filter(|(_, at)| at.elapsed() < std::time::Duration::from_secs(4));
    if let Some(t) = app.pending_reload {
        let secs = t.saturating_duration_since(std::time::Instant::now()).as_secs() + 1;
        let chip = format!(" ◍ lane edits queued · reload in ~{secs}s ");
        put_right(buf, area.right().saturating_sub(1), y, &truncate(&chip, area.width), theme::fg(theme::ARMED));
    } else if let Some((msg, _)) = toast_fresh {
        let color = if msg.starts_with('⚠') { theme::PERSISTENT } else { theme::DIRECT };
        let m = truncate(&format!(" {msg} "), area.width);
        put_right(buf, area.right().saturating_sub(1), y, &m, theme::fg(color));
    } else if app.search_committed() {
        draw_search_indicator(buf, area, y, x, app);
    }
}

/// The `/`-editor footer: a `/` prompt, the pattern with a block cursor (reversed
/// cell), and a right-aligned key hint. Full mid-string editing lives in `app`.
fn draw_search_editor(buf: &mut Buffer, area: Rect, y: u16, app: &App) {
    let left = area.left();
    let base = theme::fg(theme::BRIGHT);
    let cursor_st = base.add_modifier(Modifier::REVERSED);
    put(buf, left, y, "/", theme::bold(theme::BRIGHT));
    let chars: Vec<char> = app.search.buf.chars().collect();
    let cur = app.search.cursor.min(chars.len());
    let mut x = left + 1;
    for (i, ch) in chars.iter().enumerate() {
        let mut b = [0u8; 4];
        put(buf, x, y, ch.encode_utf8(&mut b), if i == cur { cursor_st } else { base });
        x += 1;
    }
    if cur >= chars.len() {
        put(buf, x, y, " ", cursor_st); // block cursor sitting past the last char
        x += 1;
    }
    // Right-aligned key hint (only if it clears the typed text).
    let hint = " regex · ↵ apply · esc cancel ";
    let start = area.right().saturating_sub(dw(hint));
    if start > x + 1 {
        put_right(buf, area.right().saturating_sub(1), y, hint, theme::fg(theme::DIMMER));
    }
}

/// The committed-search indicator, right-aligned, with the space-adaptive degrade
/// ladder (METRICS.md): full `/pat/ (n/m)` when it fits — or for 10 s after the
/// pattern last changed even if it collides with the hints — then it retreats to
/// `(n/m)`, then hides entirely if even that would collide. `hints_end` is the
/// first free column past the left-hand hints.
fn draw_search_indicator(buf: &mut Buffer, area: Rect, y: u16, hints_end: u16, app: &App) {
    let (n, m) = app.search_counts();
    let pat = truncate(&app.search.committed, 40);
    let full = format!(" /{pat}/ ({n}/{m}) ");
    let reduced = format!(" ({n}/{m}) ");
    let right = area.right().saturating_sub(1);
    // start column if right-aligned; fits when it clears the hints by ≥1 cell.
    let fits = |s: &str| right.saturating_sub(dw(s).saturating_sub(1)) > hints_end;
    let within10 = app.search.changed_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(10));
    let st = theme::fg(theme::BRIGHT);
    if fits(&full) || within10 {
        // Blocks the hint tail during the 10 s echo window when it doesn't fit.
        put_right(buf, right, y, &truncate(&full, area.width), st);
    } else if fits(&reduced) {
        put_right(buf, right, y, &reduced, st);
    }
    // else: even `(n/m)` would collide — hidden; the hints win.
}
