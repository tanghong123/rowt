//! rowt monitor — a read-only TUI for observing a running rowt proxy.
//!
//! Usage:
//!   rowt-monitor            run the live TUI
//!   rowt-monitor --render WxH   print one fixture frame as plain text (for
//!                               diffing against the golden renders) and exit
//!   rowt-monitor --fixtures     force the fixture source (offline demo)
//!   rowt-monitor --version / --help

use std::io::stdout;
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
use rowt_monitor::{input, render_text, ui};

const DATA_TICK: Duration = Duration::from_secs(2);
// Redraw cadence for the pulse / marquee. ~14 fps keeps the breathing dot smooth
// without busy-spinning; ratatui only repaints changed cells.
const ANIM_TICK: Duration = Duration::from_millis(70);

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut force_fixtures = false;
    let mut render: Option<String> = None;
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
            "--render" => {
                i += 1;
                render = Some(args.get(i).cloned().unwrap_or_default());
            }
            other => {
                eprintln!("rowt-monitor: unknown argument '{}'", other);
                print_help();
                std::process::exit(2);
            }
        }
        i += 1;
    }

    if let Some(spec) = render {
        return render_frame(&spec);
    }

    let source: Box<dyn Source> = if force_fixtures {
        Box::new(FixtureSource::new())
    } else {
        Box::new(LiveSource::new())
    };
    run(App::new(source))
}

fn print_help() {
    println!(
        "rowt monitor — read-only proxy observer\n\n\
         USAGE:\n  rowt-monitor [--fixtures] [--render WxH] [--version]\n\n\
         KEYS: ↑↓/jk move · ←→/hl pane · Tab focus · f lane · w window · y copy · p pause · ? help · q quit"
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

fn run(mut app: App) -> Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
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
    execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

fn event_loop<B: Backend>(term: &mut Terminal<B>, app: &mut App) -> Result<()> {
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
        app.strip_page = hit.strip_page;

        if event::poll(ANIM_TICK)? {
            match event::read()? {
                Event::Key(k) => {
                    if let Some(action) = input::key(k, app) {
                        app.update(action);
                    }
                }
                Event::Mouse(m) => match m.kind {
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
