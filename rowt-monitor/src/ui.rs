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

    // ---- outer frame ----
    put(buf, x0, y0, "╭", border);
    hfill(buf, x0 + 1, x0 + w - 2, y0, '─', border);
    put(buf, x0 + w - 1, y0, "╮", border);
    for y in (y0 + 1)..(y0 + h - 1) {
        put(buf, x0, y, "│", border);
        put(buf, x0 + w - 1, y, "│", border);
    }
    put(buf, x0, y0 + h - 1, "╰", border);
    hfill(buf, x0 + 1, x0 + w - 2, y0 + h - 1, '─', border);
    put(buf, x0 + w - 1, y0 + h - 1, "╯", border);

    draw_identity(buf, x0, y0, app, present);

    // ---- vertical layout ----
    let side = w >= 130;
    let health_top = y0 + h - 5;
    let panes_top = y0 + 6;
    let panes_bot = health_top.saturating_sub(2); // blank row sits at health_top-1

    let xl = x0 + 2; // inner box left border
    let xr = x0 + w - 3; // inner box right border

    let mut hit = Hit {
        side_by_side: side,
        ..Default::default()
    };

    if side {
        // One split box. Divider column (see brief): connections/errors leftover
        // splits ~5:2; errors held to >= 1/3 of interior.
        let interior = (xr - 1) - (xl + 1) + 1; // content cells between borders
        let err_w = split_err_width(interior);
        let div = xr - 1 - err_w; // divider column
        draw_box_frame(buf, xl, xr, panes_top, panes_bot, Some(div), border);
        // captions
        draw_caption(buf, xl, panes_top, "live · connections", app, present, Focus::Conn, border);
        draw_caption(buf, div, panes_top, "errors & blocked", app, present, Focus::Err, border);

        let hdr_y = panes_top + 1;
        let col_y = panes_top + 6;
        let list_y = panes_top + 7;
        let list_h = panes_bot.saturating_sub(list_y) as usize; // rows before bottom border

        // left pane
        let lx0 = xl + 1;
        let lw = div - lx0;
        draw_conn_pane(buf, lx0, lw, hdr_y, col_y, list_y, list_h, app, present, &mut hit);
        // right pane
        let rx0 = div + 1;
        let rw = (xr - 1) - rx0 + 1;
        draw_err_pane(buf, rx0, rw, hdr_y, col_y, list_y, list_h, app, present, &mut hit);
    } else {
        // Stacked: connections box, blank, errors box. Split list rows evenly.
        let total = panes_bot as i32 - panes_top as i32 + 1;
        let list_total = (total - 17).max(2);
        let r1 = ((list_total + 1) / 2) as u16;
        let r2 = (list_total / 2) as u16;

        // connections box
        let c_top = panes_top;
        let c_bot = c_top + 7 + r1;
        draw_box_frame(buf, xl, xr, c_top, c_bot, None, border);
        draw_caption(buf, xl, c_top, "live · connections", app, present, Focus::Conn, border);
        let cx0 = xl + 1;
        let cw = (xr - 1) - cx0 + 1;
        draw_conn_pane(buf, cx0, cw, c_top + 1, c_top + 6, c_top + 7, r1 as usize, app, present, &mut hit);

        // errors box
        let e_top = c_bot + 2; // blank row between
        let e_bot = panes_bot;
        draw_box_frame(buf, xl, xr, e_top, e_bot, None, border);
        draw_caption(buf, xl, e_top, "errors & blocked", app, present, Focus::Err, border);
        draw_err_pane(buf, cx0, cw, e_top + 1, e_top + 6, e_top + 7, r2 as usize, app, present, &mut hit);
    }

    draw_health(buf, xl, xr, health_top, app, present, border);

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

fn draw_identity(buf: &mut Buffer, x0: u16, y0: u16, app: &App, present: bool) {
    let logo_st = theme::bold(theme::ESCAPE);
    for (i, line) in LOGO.iter().enumerate() {
        put(buf, x0 + 2, y0 + 1 + i as u16, line, logo_st);
    }
    let dimmer = theme::fg(theme::DIMMER);
    let bright = theme::fg(theme::BRIGHT);

    // Row 1: MONITOR
    put(buf, x0 + 29, y0 + 1, "MONITOR", dimmer);

    // Row 2: status dot, in priority order:
    //   DOWN  (red)    — router/clash API unreachable
    //   PAUSED (gray)  — sampling frozen by the user
    //   ERROR (orange) — router up but the active server (or, in auto mode, the
    //                    whole pool) is failing its probe
    //   LIVE  (green)  — healthy; breathes to show sampling is live
    // Present/golden mode is always the neutral LIVE dot.
    let id = &app.snap.identity;
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
    put(buf, x0 + 46, y0 + 2, &app.snap.identity.mode, bright);
    put(buf, x0 + 70, y0 + 2, "uptime", dimmer);
    put(buf, x0 + 78, y0 + 2, &app.snap.identity.uptime, bright);

    // Row 3: server / router
    put(buf, x0 + 37, y0 + 3, "server", dimmer);
    // Reserve name width from the pool's longest name so the ms column is
    // stable; bounded (<=13) so it never runs into the router column at 70.
    // 8 reproduces the golden (JP-Tokyo -> ms at col 55).
    let reserve = id.name_reserve.clamp(6, 13);
    // Gray out the server name unless it's confirmed reachable (present = ok).
    let name_st = if present || id.active_ok == Some(true) {
        theme::bold(theme::ESCAPE)
    } else {
        theme::fg(theme::DIM)
    };
    put(buf, x0 + 46, y0 + 3, &truncate(&id.server_name, reserve), name_st);
    // Latency, or "—" when there is no reading (router down / not probed).
    let (ms, ms_st) = match id.server_ms {
        Some(v) => (format!("{} ms", v), theme::bold(theme::latency_color(v))),
        None => ("—".to_string(), theme::fg(theme::DIM)),
    };
    put(buf, x0 + 47 + reserve, y0 + 3, &ms, ms_st);
    put(buf, x0 + 70, y0 + 3, "router", dimmer);
    put(buf, x0 + 78, y0 + 3, &app.snap.identity.router, bright);

    // Row 4: proxy / config
    put(buf, x0 + 37, y0 + 4, "proxy", dimmer);
    put(buf, x0 + 46, y0 + 4, &app.snap.identity.proxy, bright);
    put(buf, x0 + 70, y0 + 4, "config", dimmer);
    put(buf, x0 + 78, y0 + 4, &app.snap.identity.config, bright);
}

/// Draw box borders + captions row / rule row / bottom row for a box spanning
/// columns [xl..xr], rows [top..bot]. `div` adds a vertical rule.
fn draw_box_frame(buf: &mut Buffer, xl: u16, xr: u16, top: u16, bot: u16, div: Option<u16>, border: Style) {
    // top and captions are drawn by draw_caption; here we do the horizontal top
    // fill baseline (caption overwrites the left part) plus corners.
    put(buf, xl, top, "╭", border);
    hfill(buf, xl + 1, xr - 1, top, '─', border);
    put(buf, xr, top, "╮", border);
    if let Some(d) = div {
        put(buf, d, top, "┬", border);
    }

    // rule row is 5 rows below top (after 4 header rows)
    let rule = top + 5;
    put(buf, xl, rule, "├", border);
    hfill(buf, xl + 1, xr - 1, rule, '─', border);
    put(buf, xr, rule, "┤", border);
    if let Some(d) = div {
        put(buf, d, rule, "┼", border);
    }

    // vertical borders for every content row (the rule row already has ├─┼─┤)
    for y in (top + 1)..bot {
        if y == rule {
            continue;
        }
        put(buf, xl, y, "│", border);
        put(buf, xr, y, "│", border);
        if let Some(d) = div {
            put(buf, d, y, "│", border);
        }
    }

    // bottom
    put(buf, xl, bot, "╰", border);
    hfill(buf, xl + 1, xr - 1, bot, '─', border);
    put(buf, xr, bot, "╯", border);
    if let Some(d) = div {
        put(buf, d, bot, "┴", border);
    }
}

/// Draw `─┤ caption ├` starting just after the corner/divider at `corner`, in
/// focus-aware styling.
#[allow(clippy::too_many_arguments)]
fn draw_caption(buf: &mut Buffer, corner: u16, y: u16, label: &str, app: &App, present: bool, which: Focus, border: Style) {
    // The ┤ ├ connectors always match the border; focus is shown by brightening
    // the caption text only, so the connectors never look out of step.
    let focused = !present && app.focus == which;
    let cstyle = if focused { theme::bold(theme::BRIGHT) } else { theme::fg(theme::DIMMER) };
    let mut x = corner + 1;
    put(buf, x, y, "─┤ ", border);
    x += 3;
    put(buf, x, y, label, cstyle);
    x += dw(label);
    // active lane filter chip in the connections caption
    if which == Focus::Conn {
        if let Some(l) = app.lane_filter {
            let chip = format!(" · {}", l.label());
            put(buf, x, y, &chip, theme::fg(l.color()));
            x += dw(&chip);
        }
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

    // Header rate table: all + escape/corp/direct.
    let rows: Vec<(Option<Lane>, f64, f64, u32, Style)> = {
        let mut v = vec![(
            None,
            app.snap.all.up,
            app.snap.all.down,
            app.snap.all.conns,
            theme::bold(theme::BRIGHT),
        )];
        for la in &app.snap.lanes {
            v.push((Some(la.lane), la.up, la.down, la.conns, theme::bold(la.lane.color())));
        }
        v
    };
    for (i, (lane, u, d, n, name_st)) in rows.iter().enumerate() {
        let y = hdr_y + i as u16;
        if y >= col_y {
            break;
        }
        let name = lane.map_or("all", |l| l.label());
        put(buf, x0 + 1, y, name, *name_st);
        let val_st = if lane.is_none() { theme::bold(theme::BRIGHT) } else { dim };
        // No connections in this row -> no meaningful rate; show "—".
        let idle = *n == 0;
        put(buf, x0 + 8, y, "↑", up);
        if idle {
            put(buf, x0 + 10, y, "—", dim);
        } else {
            let (uv, uu) = format::rate_parts(*u);
            put(buf, x0 + 10, y, &format!("{} {}", uv, uu), val_st);
        }
        put(buf, x0 + 21, y, "↓", down);
        if idle {
            put(buf, x0 + 23, y, "—", dim);
        } else {
            let (dv, du) = format::rate_parts(*d);
            put(buf, x0 + 23, y, &format!("{} {}", dv, du), val_st);
        }
        let conn = format!("{} conn", n);
        let conn_st = if lane.is_none() { theme::bold(theme::BRIGHT) } else { dimmer };
        put_right(buf, x0 + w - 2, y, &conn, conn_st);
        hit.lanes.push((Rect::new(x0 + 1, y, w - 2, 1), *lane));
    }

    // Column header.
    put(buf, x0 + 1, col_y, "LANE", dimmer);
    put(buf, x0 + 8, col_y, "HOST:PORT", dimmer);
    put_right(buf, x0 + w - 38, col_y, "#", dimmer);
    put_right(buf, x0 + w - 27, col_y, "UP", dimmer);
    put_right(buf, x0 + w - 17, col_y, "DOWN", dimmer);
    put(buf, x0 + w - 14, col_y, "RULE", dimmer);

    // Data rows.
    let view = app.conns_view();
    let scroll = app.conn_scroll.min(view.len().saturating_sub(1));
    hit.conn_list = Rect::new(x0, list_y, w, list_h as u16);
    hit.conn_pane = Rect::new(x0, hdr_y, w, (list_y + list_h as u16).saturating_sub(hdr_y));
    hit.conn_h = list_h;
    let host_max = (w.saturating_sub(38)).saturating_sub(9); // host col8 .. before '#'
    for row in 0..list_h {
        let idx = scroll + row;
        if idx >= view.len() {
            break;
        }
        let c = view[idx];
        let y = list_y + row as u16;
        put(buf, x0 + 1, y, c.lane.label(), theme::bold(c.lane.color()));
        let hostport = format!("{}:{}", c.host, c.port);
        let selected = !present && app.focus == Focus::Conn && idx == app.conn_sel;
        let shown = if selected {
            marquee(&hostport, host_max, app.marquee)
        } else {
            truncate(&hostport, host_max)
        };
        put(buf, x0 + 8, y, &shown, theme::fg(theme::BRIGHT));
        put_right(buf, x0 + w - 38, y, &c.conns.to_string(), dimmer);
        put_right(buf, x0 + w - 27, y, &format!("↑{}", format::compact(c.up)), theme::fg(theme::UP_TABLE));
        put_right(buf, x0 + w - 17, y, &format!("↓{}", format::compact(c.down)), theme::fg(theme::DOWN_TABLE));
        put(buf, x0 + w - 14, y, &c.rule, dimmer);
        if selected {
            highlight_row(buf, x0, y, w, c.lane.color());
        }
    }
    if !present {
        draw_scrollbar(buf, x0 + w - 1, list_y, list_h, view.len(), scroll);
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

    // Data rows.
    let errs = &app.snap.errors;
    let scroll = app.err_scroll.min(errs.len().saturating_sub(1));
    hit.err_list = Rect::new(x0, list_y, w, list_h as u16);
    hit.err_pane = Rect::new(x0, hdr_y, w, (list_y + list_h as u16).saturating_sub(hdr_y));
    hit.err_h = list_h;
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
        let selected = !present && app.focus == Focus::Err && idx == app.err_sel;
        let shown = if selected {
            marquee(&e.domain, dom_max, app.marquee)
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

fn draw_health(buf: &mut Buffer, xl: u16, xr: u16, top: u16, app: &App, present: bool, border: Style) {
    let _ = present;
    draw_box_frame_health(buf, xl, xr, top, border);
    // Caption (server health is never a focus target, so always neutral).
    put(buf, xl + 1, top, "─┤ ", border);
    put(buf, xl + 4, top, "server health", theme::fg(theme::DIMMER));
    put(buf, xl + 4 + dw("server health"), top, " ├", border);

    let x0 = xl + 1;
    let w = (xr - 1) - x0 + 1;
    let s = &app.snap;
    // Stats: the active server is named in the header identity band and marked
    // in the strip below, so it is not repeated here.
    let stats = format!("{} servers · {} up · {} down", s.servers_total, s.servers_up, s.servers_down);
    put(buf, x0 + 1, top + 1, &stats, theme::fg(theme::DIM));

    // chips row (marquee as a whole)
    draw_chips(buf, x0 + 1, top + 2, w.saturating_sub(2), app, present);
}

fn draw_box_frame_health(buf: &mut Buffer, xl: u16, xr: u16, top: u16, border: Style) {
    put(buf, xl, top, "╭", border);
    hfill(buf, xl + 1, xr - 1, top, '─', border);
    put(buf, xr, top, "╮", border);
    for y in (top + 1)..(top + 3) {
        put(buf, xl, y, "│", border);
        put(buf, xr, y, "│", border);
    }
    put(buf, xl, top + 3, "╰", border);
    hfill(buf, xl + 1, xr - 1, top + 3, '─', border);
    put(buf, xr, top + 3, "╯", border);
}

fn draw_chips(buf: &mut Buffer, x0: u16, y: u16, w: u16, app: &App, present: bool) {
    let bright = theme::fg(theme::BRIGHT);
    let escape = theme::fg(theme::ESCAPE);
    // Each chip is a run of styled segments. The active server leads with a ▶.
    let chips: Vec<Vec<(String, Style)>> = app
        .snap
        .chips
        .iter()
        .map(|c| {
            let lat = theme::fg(theme::latency_color(c.ms));
            let ms = format!("{:>3} ms", c.ms);
            if c.active {
                vec![
                    ("▶ ".to_string(), escape),
                    (c.name.clone(), theme::bold(theme::ESCAPE)),
                    (" ".to_string(), bright),
                    (ms, lat),
                ]
            } else {
                vec![(c.name.clone(), bright), (" ".to_string(), bright), (ms, lat)]
            }
        })
        .collect();
    let widths: Vec<u16> = chips
        .iter()
        .map(|segs| segs.iter().map(|(s, _)| dw(s)).sum())
        .collect();
    let total: u16 = widths.iter().sum::<u16>() + 3 * (chips.len().saturating_sub(1) as u16);

    if present || total <= w {
        // Pack complete chips left-to-right; stop before one that won't fit.
        let mut col = x0;
        for (i, segs) in chips.iter().enumerate() {
            let sep = if i > 0 { 3 } else { 0 };
            if col + sep + widths[i] > x0 + w {
                break;
            }
            col += sep;
            for (s, st) in segs {
                put(buf, col, y, s, *st);
                col += dw(s);
            }
        }
    } else {
        // Overflowing: marquee the whole strip (cell-level scroll).
        let mut cells: Vec<(char, Style)> = Vec::new();
        for (i, segs) in chips.iter().enumerate() {
            if i > 0 {
                cells.extend([(' ', Style::default()); 3]);
            }
            for (s, st) in segs {
                cells.extend(s.chars().map(|c| (c, *st)));
            }
        }
        let span = cells.len() + 3;
        let off = (app.marquee / 2) % span;
        for k in 0..w as usize {
            let ci = off + k;
            let (ch, st) = if ci % span < cells.len() {
                cells[ci % span]
            } else {
                (' ', Style::default())
            };
            let mut b = [0u8; 4];
            put(buf, x0 + k as u16, y, ch.encode_utf8(&mut b), st);
        }
    }
}

// ---------------- helpers ----------------

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

/// Horizontal auto-scroll of an overflowing value (selected row only).
fn marquee(s: &str, max: u16, phase: usize) -> String {
    let width = dw(s);
    if width <= max {
        return s.to_string();
    }
    let pad = 3u16;
    let span = width + pad; // s + gap
    let off = ((phase / 2) as u16) % span;
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
        "  rowt monitor — keys                    ",
        "                                         ",
        "  ↑↓ / j k   move selection              ",
        "  ←→ / h l   switch pane (side by side)  ",
        "  Tab        cycle focus                 ",
        "  f 1 2 3 0  lane filter / jump / clear  ",
        "  w [ ]      errors window               ",
        "  y          yank domain / host:port     ",
        "  r          re-probe servers now        ",
        "  p          pause sampling              ",
        "  ?          toggle this help            ",
        "  q          quit                        ",
    ];
    let bw = lines[0].chars().count() as u16;
    let bh = lines.len() as u16 + 2;
    let bx = area.left() + (area.width.saturating_sub(bw)) / 2;
    let by = area.top() + (area.height.saturating_sub(bh)) / 2;
    let border = theme::fg(theme::BORDER_FOCUS);
    put(buf, bx, by, "╭", border);
    hfill(buf, bx + 1, bx + bw, by, '─', border);
    put(buf, bx + bw + 1, by, "╮", border);
    for (i, l) in lines.iter().enumerate() {
        let y = by + 1 + i as u16;
        put(buf, bx, y, "│", border);
        put(buf, bx + 1, y, l, theme::fg(theme::BRIGHT));
        put(buf, bx + bw + 1, y, "│", border);
    }
    let yb = by + bh - 1;
    put(buf, bx, yb, "╰", border);
    hfill(buf, bx + 1, bx + bw, yb, '─', border);
    put(buf, bx + bw + 1, yb, "╯", border);
}

/// The bottom hint bar (interactive only; not part of the golden frame).
pub fn draw_footer(buf: &mut Buffer, area: Rect, app: &App) {
    let dimmer = theme::fg(theme::DIMMER);
    let y = area.bottom().saturating_sub(1);
    let hint = if app.paused {
        " ↑↓ move · ←→ pane · f lane · w window · y copy · r reprobe · p resume · ? help · q quit "
    } else {
        " ↑↓ move · ←→ pane · f lane · w window · y copy · r reprobe · p pause · ? help · q quit "
    };
    let shown = truncate(hint, area.width);
    put(buf, area.left(), y, &shown, dimmer);
    if let Some(yk) = &app.last_yank {
        let msg = format!(" copied {} ", yk);
        let m = truncate(&msg, area.width);
        put_right(buf, area.right().saturating_sub(1), y, &m, theme::fg(theme::DIRECT));
    }
}
