//! rowt monitor — a read-only TUI for observing a running rowt proxy.
//!
//! Usage:
//!   rowt-monitor            run the live TUI
//!   rowt-monitor --render WxH   print one fixture frame as plain text (for
//!                               diffing against the golden renders) and exit
//!   rowt-monitor --fixtures     force the fixture source (offline demo)
//!   rowt-monitor --theme T      dark | light | auto (default auto)
//!   rowt-monitor --version / --help

use std::io::{stdout, Write};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, MouseButton, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;
use ratatui::Terminal;

use rowt_monitor::app::App;
use rowt_monitor::source::{FixtureSource, LiveSource, Source};
use rowt_monitor::theme::{self, ThemeArg};
use rowt_monitor::{input, render_text, ui};

const DATA_TICK: Duration = Duration::from_secs(2);
// Redraw cadence for the pulse / marquee. ~14 fps keeps the breathing dot smooth
// without busy-spinning; ratatui only repaints changed cells.
const ANIM_TICK: Duration = Duration::from_millis(70);

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut force_fixtures = false;
    let mut render: Option<String> = None;
    let mut render_ansi_spec: Option<String> = None;
    // `--theme` wins over `ROWT_MONITOR_THEME` (set it in a profile to pin a
    // terminal that answers OSC 11 wrongly); both default to auto-detect.
    let mut theme_arg = match std::env::var("ROWT_MONITOR_THEME") {
        Ok(v) => ThemeArg::parse(&v).unwrap_or_else(|| {
            eprintln!("rowt-monitor: ignoring ROWT_MONITOR_THEME='{v}' (want dark|light|auto)");
            ThemeArg::Auto
        }),
        Err(_) => ThemeArg::Auto,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "-V" => {
                println!("rowt-monitor {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            "--fixtures" | "--demo" => force_fixtures = true,
            "--theme" => {
                i += 1;
                let v = args.get(i).cloned().unwrap_or_default();
                match ThemeArg::parse(&v) {
                    Some(t) => theme_arg = t,
                    None => {
                        eprintln!("rowt-monitor: bad --theme '{v}', want dark|light|auto");
                        std::process::exit(2);
                    }
                }
            }
            "--render" => {
                i += 1;
                render = Some(args.get(i).cloned().unwrap_or_default());
            }
            "--render-ansi" => {
                i += 1;
                render_ansi_spec = Some(args.get(i).cloned().unwrap_or_default());
            }
            other => {
                eprintln!("rowt-monitor: unknown argument '{}'", other);
                print_help();
                std::process::exit(2);
            }
        }
        i += 1;
    }

    // Headless renders go through a pipe, where there's no terminal to ask and
    // nothing to be legible on: `auto` keeps the frozen dark palette so golden
    // diffs are stable. An explicit `--theme` still applies, so the light palette
    // can be rendered and diffed too. The live path resolves inside `run`, once
    // raw mode is on — an OSC 11 reply is line-buffered and echoed without it.
    let headless = render.is_some() || render_ansi_spec.is_some();
    if headless && theme_arg != ThemeArg::Auto {
        theme::set(theme::resolve(theme_arg));
    }
    if let Some(spec) = render {
        return render_frame(&spec);
    }
    if let Some(spec) = render_ansi_spec {
        let (w, h) = parse_wh(&spec).ok_or_else(|| anyhow::anyhow!("bad --render-ansi spec '{}', want WxH", spec))?;
        print!("{}", rowt_monitor::render_ansi(w, h));
        return Ok(());
    }

    let source: Box<dyn Source> = if force_fixtures {
        Box::new(FixtureSource::new())
    } else {
        Box::new(LiveSource::new())
    };
    run(App::new(source), theme_arg)
}

fn print_help() {
    println!(
        "rowt monitor — read-only proxy observer\n\n\
         USAGE:\n  rowt-monitor [--theme dark|light|auto] [--fixtures] [--render WxH] [--render-ansi WxH] [--version]\n\n\
         THEME: --theme auto (default) reads the terminal's background — COLORFGBG, then an\n\
         OSC 11 query — and picks the light palette only for a near-paper background, else dark.\n\
         Pin it with --theme dark|light, or ROWT_MONITOR_THEME.\n\n\
         KEYS: ↑↓/jk move · ←→/hl pane · Tab focus · v flip · s span · f lane · / search · w window · y copy · p pause · ? help · q quit"
    );
}

/// Render one fixture frame at WxH to plain text (glyphs only) and print it.
fn render_frame(spec: &str) -> Result<()> {
    let (w, h) = parse_wh(spec).ok_or_else(|| anyhow::anyhow!("bad --render spec '{}', want WxH", spec))?;
    print!("{}", render_text(w, h));
    Ok(())
}

fn parse_wh(spec: &str) -> Option<(u16, u16)> {
    let (a, b) = spec.split_once(['x', 'X', '×'])?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn run(mut app: App, theme_arg: ThemeArg) -> Result<()> {
    enable_raw_mode()?;
    // Resolve the palette here, not in `main`: `auto` may query the terminal's
    // background over /dev/tty, and that reply only comes back unbuffered and
    // unechoed in raw mode. Still before the alternate screen, so nothing drawn
    // can be disturbed if the terminal echoes something unexpected.
    theme::set(theme::resolve(theme_arg));
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    // crossterm's mouse capture only reports motion while a button is held; also
    // request any-motion reporting (1003h) so we get hover events for the
    // clickable identity-band regions. Disabled again in restore().
    write!(out, "\x1b[?1003h")?;
    out.flush()?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;

    // Restore the terminal even if we panic.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        default_hook(info);
    }));

    let res = event_loop(&mut term, &mut app);

    restore()?;
    res
}

/// Read the glyphs covered by a single-row drag selection out of the buffer.
fn read_span(buf: &ratatui::buffer::Buffer, d: rowt_monitor::app::Drag) -> String {
    let mut s = String::new();
    for x in d.lo()..=d.hi() {
        if let Some(c) = buf.cell((x, d.row)) {
            s.push_str(c.symbol());
        }
    }
    s
}

fn restore() -> Result<()> {
    disable_raw_mode()?;
    let mut o = stdout();
    let _ = write!(o, "\x1b[?1003l"); // stop any-motion reporting
    let _ = o.flush();
    execute!(o, LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

// ratatui 0.30 made `Backend::Error` an associated type, so `term.draw(…)?`
// only converts into `anyhow::Error` when that type is Send + Sync. Naming the
// bound is the whole of the upgrade for us; `CrosstermBackend`'s error is
// `io::Error`, which satisfies it.
fn event_loop<B: Backend>(term: &mut Terminal<B>, app: &mut App) -> Result<()>
where
    <B as Backend>::Error: Send + Sync + 'static,
{
    let mut last_data = Instant::now();
    let mut hit = ui::Hit::default();
    loop {
        term.draw(|f| {
            let area = f.area();
            // reserve the last row for the footer hint bar
            let frame = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
            let buf = f.buffer_mut();
            hit = ui::draw(buf, frame, app, false);
            ui::draw_footer(buf, area, app);
        })?;
        // feed geometry back so movement clamps correctly
        app.conn_h = hit.conn_h.max(1);
        app.err_h = hit.err_h.max(1);
        app.side_by_side = hit.side_by_side;
        app.feed_strip(&hit);

        if event::poll(ANIM_TICK)? {
            match event::read()? {
                Event::Key(k) => {
                    if let Some(action) = input::key(k, app) {
                        app.update(action);
                    }
                }
                Event::Mouse(m) => match m.kind {
                    // Track the pointer for hover highlights (e.g. sys proxy).
                    MouseEventKind::Moved => app.hover = Some((m.column, m.row)),
                    MouseEventKind::Drag(MouseButton::Left) => {
                        // Extend a single-row selection (anchored on the first
                        // drag event's row) and cancel any prior yank toast.
                        let d = app.drag.get_or_insert(rowt_monitor::app::Drag {
                            row: m.row,
                            c0: m.column,
                            c1: m.column,
                        });
                        d.c1 = m.column;
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        if let Some(d) = app.drag.take() {
                            let text = read_span(term.current_buffer_mut(), d);
                            let t = text.trim();
                            if !t.is_empty() {
                                rowt_monitor::clipboard::copy(t);
                                app.notify(format!("copied {}", t));
                                app.last_yank = Some(t.to_string());
                            }
                        }
                    }
                    _ => {
                        if let Some(action) = input::mouse(m, &hit) {
                            app.update(action);
                        }
                    }
                },
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
        if app.should_quit {
            return Ok(());
        }
        // Per-frame: deliver control outcomes, fire the debounced reload, expire
        // stale arms (runs every ANIM_TICK, independent of the 2s data tick).
        app.on_frame();
        if last_data.elapsed() >= DATA_TICK {
            app.tick();
            last_data = Instant::now();
        }
    }
}
