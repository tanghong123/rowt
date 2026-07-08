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
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;
use ratatui::Terminal;

use rowt_monitor::app::App;
use rowt_monitor::source::{FixtureSource, LiveSource, Source};
use rowt_monitor::{input, render_text, ui};

const DATA_TICK: Duration = Duration::from_secs(2);
const ANIM_TICK: Duration = Duration::from_millis(120);

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

        if event::poll(ANIM_TICK)? {
            match event::read()? {
                Event::Key(k) => {
                    if let Some(action) = input::key(k, app) {
                        app.update(action);
                    }
                }
                Event::Mouse(m) => {
                    if let Some(action) = input::mouse(m, &hit) {
                        app.update(action);
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
        app.anim();
        if app.should_quit {
            return Ok(());
        }
        if last_data.elapsed() >= DATA_TICK {
            app.tick();
            last_data = Instant::now();
        }
    }
}
