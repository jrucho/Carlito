//! Carlito: the fast, practical handwritten assistant variant.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ab_glyph::FontRef;

use crate::fb::BBox;
use crate::surface::{Surface, BLACK, WHITE};
use crate::{display, help, ink, oracle, pen, power, qtfb, script, touch};

const FONT_TTF: &[u8] = include_bytes!("../fonts/DancingScript.ttf");
const PNG_PATH: &str = "/tmp/carlito-page.png";
const IDLE_COMMIT: Duration = Duration::from_millis(2800);
const REPLY_INK_R: i32 = 1;

enum State {
    Listening {
        last_pen: Option<Instant>,
    },
    Absorbing {
        stage: u32,
        next: Instant,
        region: BBox,
        rx: mpsc::Receiver<Result<String, String>>,
    },
    Thinking {
        rx: mpsc::Receiver<Result<String, String>>,
        pulse: Instant,
        blot_on: bool,
        answer: String,
        failed: bool,
    },
    Writing {
        pages: Vec<Page>,
        page: usize,
        stroke_i: usize,
        point_i: usize,
        next: Instant,
    },
    Reading {
        pages: Vec<Page>,
        page: usize,
        until: Instant,
    },
    Fading {
        stage: u32,
        next: Instant,
        region: BBox,
    },
    Help {
        panel: Option<help::Help>,
        until: Instant,
    },
}

impl State {
    fn delay_after_sleep(self, elapsed: Duration) -> Self {
        match self {
            State::Listening { last_pen } => State::Listening {
                last_pen: last_pen.map(|time| time + elapsed),
            },
            State::Absorbing {
                stage,
                next,
                region,
                rx,
            } => State::Absorbing {
                stage,
                next: next + elapsed,
                region,
                rx,
            },
            State::Thinking {
                rx,
                pulse,
                blot_on,
                answer,
                failed,
            } => State::Thinking {
                rx,
                pulse: pulse + elapsed,
                blot_on,
                answer,
                failed,
            },
            State::Writing {
                pages,
                page,
                stroke_i,
                point_i,
                next,
            } => State::Writing {
                pages,
                page,
                stroke_i,
                point_i,
                next: next + elapsed,
            },
            State::Reading { pages, page, until } => State::Reading {
                pages,
                page,
                until: until + elapsed,
            },
            State::Fading {
                stage,
                next,
                region,
            } => State::Fading {
                stage,
                next: next + elapsed,
                region,
            },
            State::Help { panel, until } => State::Help {
                panel,
                until: until + elapsed,
            },
        }
    }
}

struct Page {
    strokes: Vec<Vec<(i32, i32)>>,
    region: BBox,
}

enum Block {
    Text(String),
    Draw {
        lines: Vec<[i32; 4]>,
        caption: String,
    },
}

pub fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--oracle-test") {
        let png = args
            .get(2)
            .map(String::as_str)
            .unwrap_or("/tmp/carlito-page.png");
        std::process::exit(oracle_test(png));
    }
    if let Err(e) = run() {
        eprintln!("carlito: fatal: {e}");
        std::process::exit(1);
    }
}

fn oracle_test(png: &str) -> i32 {
    let oracle = match oracle::Oracle::spawn() {
        Ok(oracle) => oracle,
        Err(e) => {
            eprintln!("carlito oracle failed to start: {e}");
            return 1;
        }
    };
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    oracle.ask(png, tx);
    let mut answer = String::new();
    while let Ok(result) = rx.recv() {
        match result {
            Ok(chunk) => append_chunk(&mut answer, &chunk),
            Err(e) => {
                eprintln!("carlito oracle error: {e}");
                return 1;
            }
        }
    }
    println!("{answer}");
    eprintln!(
        "carlito: reply complete in {}ms ({} chars)",
        started.elapsed().as_millis(),
        answer.len()
    );
    i32::from(answer.trim().is_empty())
}

fn run() -> std::io::Result<()> {
    let font = FontRef::try_from_slice(FONT_TTF).map_err(|e| {
        eprintln!("carlito: font load failed: {e}");
        std::io::Error::other(e)
    })?;
    let (disp, mut surf) = display::Display::open().map_err(|e| {
        eprintln!("carlito: display open failed: {e}");
        e
    })?;
    let takeover = matches!(disp, display::Display::Quill);
    eprintln!(
        "carlito: display {} ({}x{} stride {})",
        if takeover { "quill/takeover" } else { "qtfb" },
        surf.w,
        surf.h,
        surf.stride
    );

    let mut pen_dev = match pen::PenDevice::open(surf.w, surf.h) {
        Ok(pen) => Some(pen),
        Err(e) => {
            eprintln!("carlito: raw pen unavailable ({e}), using qtfb pen events");
            None
        }
    };
    let mut touch_dev = if takeover {
        touch::TouchDevice::open_for_screen(surf.w, surf.h).ok()
    } else {
        None
    };
    let mut power_dev = if takeover {
        power::PowerButton::open()
            .map_err(|e| eprintln!("carlito: no power button ({e})"))
            .ok()
    } else {
        None
    };
    let mut power_grace = Instant::now();

    let sigterm = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&sigterm))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&sigterm))?;

    show_splash(&font, &mut surf);
    disp.update_all(surf.w, surf.h);
    std::thread::sleep(Duration::from_millis(1250));
    surf.fill_rect(0, 0, surf.w, surf.h, WHITE);
    disp.update_all(surf.w, surf.h);

    let oracle = match oracle::Oracle::spawn() {
        Ok(oracle) => Some(oracle),
        Err(e) => {
            eprintln!("carlito: oracle failed to start: {e}");
            None
        }
    };

    let mut user_ink = ink::Ink::new();
    let mut state = State::Listening { last_pen: None };
    let mut pen_down = false;
    let mut stylus_on = false;
    let mut stylus_tapped = false;
    let mut ink_dirty = BBox::empty();
    let mut last_flush = Instant::now();
    let flush_every = if takeover {
        Duration::from_millis(8)
    } else {
        Duration::from_millis(35)
    };
    let mut qtfb_touches = HashSet::new();
    let mut qtfb_touch_max = 0usize;
    let mut qtfb_starts: std::collections::HashMap<i32, (i32, i32, Instant)> =
        std::collections::HashMap::new();
    let mut history: Vec<String> = Vec::new();

    eprintln!("carlito: ready");
    'app: loop {
        if sigterm.load(Ordering::Relaxed) {
            break;
        }

        let mut page_tap = None;
        let mut new_chat = false;
        if let Some(ref mut touch) = touch_dev {
            let actions = touch.drain();
            if actions.quit {
                eprintln!("carlito: five-finger quit");
                break;
            }
            if actions.new_chat {
                new_chat = true;
            }
            page_tap = actions.tap;
        }

        if let Some(ref mut button) = power_dev {
            if button.drain_pressed() && Instant::now() >= power_grace {
                let elapsed = sleep_and_restore(
                    button,
                    &disp,
                    &mut surf,
                    &font,
                    &mut pen_dev,
                    &mut touch_dev,
                );
                state = state.delay_after_sleep(elapsed);
                pen_down = false;
                stylus_on = false;
                user_ink.pen_up();
                power_grace = Instant::now() + Duration::from_secs(3);
            }
        }

        if let Some(ref mut device) = pen_dev {
            for sample in device.drain() {
                let writing = sample.touching && sample.pressure > 40;
                stylus_on = writing;
                stylus_tapped |= writing;
                if !writing {
                    if pen_down {
                        pen_down = false;
                        user_ink.pen_up();
                        if let State::Listening { ref mut last_pen } = state {
                            *last_pen = Some(Instant::now());
                        }
                    }
                    continue;
                }
                match state {
                    State::Listening { ref mut last_pen } => {
                        pen_down = true;
                        let dirty = match sample.tool {
                            pen::Tool::Pen => {
                                let radius = 2 + sample.pressure * 2 / pen::MAX_PRESSURE;
                                user_ink.pen_point(&mut surf, sample.x, sample.y, radius)
                            }
                            pen::Tool::Eraser => {
                                user_ink.erase_point(&mut surf, sample.x, sample.y, 22)
                            }
                        };
                        if !dirty.is_empty() {
                            ink_dirty.add(dirty.x0, dirty.y0, 0);
                            ink_dirty.add(dirty.x1, dirty.y1, 0);
                        }
                        *last_pen = Some(Instant::now());
                    }
                    State::Reading {
                        ref pages, page, ..
                    } => {
                        state = State::Fading {
                            stage: 0,
                            next: Instant::now(),
                            region: pages[page].region,
                        };
                    }
                    _ => {}
                }
            }
        }

        let events = match disp.pump() {
            Ok(events) => events,
            Err(_) => break,
        };
        for event in events {
            // Debug: log all qtfb events on RM2
            if !takeover {
                eprintln!(
                    "carlito: qtfb evt type={} dev={} x={} y={} d={} state={}",
                    event.input_type,
                    event.dev_id,
                    event.x,
                    event.y,
                    event.d,
                    match &state {
                        State::Listening { .. } => "Listening",
                        State::Absorbing { .. } => "Absorbing",
                        State::Thinking { .. } => "Thinking",
                        State::Writing { .. } => "Writing",
                        State::Reading { .. } => "Reading",
                        State::Fading { .. } => "Fading",
                        State::Help { .. } => "Help",
                    }
                );
            }
            match event.input_type {
                qtfb::INPUT_TOUCH_PRESS => {
                    qtfb_touches.insert(event.dev_id);
                    qtfb_starts.insert(event.dev_id, (event.x, event.y, Instant::now()));
                    qtfb_touch_max = qtfb_touch_max.max(qtfb_touches.len());
                    if qtfb_touch_max >= 5 {
                        eprintln!("carlito: five-finger qtfb quit");
                        break 'app;
                    }
                    continue;
                }
                qtfb::INPUT_TOUCH_UPDATE => continue,
                qtfb::INPUT_TOUCH_RELEASE => {
                    let start = qtfb_starts.remove(&event.dev_id);
                    qtfb_touches.remove(&event.dev_id);
                    if qtfb_touch_max == 2 {
                        if let Some((sx, sy, t)) = start {
                            let dy = event.y - sy;
                            let dx = (event.x - sx).abs();
                            let elapsed = t.elapsed();
                            let swipe_thresh = (surf.h as i32 / 6).max(180);
                            if dy > swipe_thresh
                                && dx * 2 < dy
                                && elapsed >= Duration::from_millis(80)
                                && elapsed <= Duration::from_millis(1200)
                            {
                                new_chat = true;
                            }
                        }
                    } else if qtfb_touch_max <= 1 {
                        page_tap = Some((event.x, event.y));
                    }
                    if qtfb_touches.is_empty() {
                        qtfb_touch_max = 0;
                    }
                    continue;
                }
                _ => {}
            }
            if pen_dev.is_some() {
                continue;
            }
            match event.input_type {
                qtfb::INPUT_PEN_PRESS | qtfb::INPUT_PEN_UPDATE => {
                    stylus_on = true;
                    stylus_tapped = true;
                    if let State::Listening { ref mut last_pen } = state {
                        pen_down = true;
                        let radius = 2 + event.d.clamp(0, 100) / 60;
                        let dirty = user_ink.pen_point(&mut surf, event.x, event.y, radius);
                        if !dirty.is_empty() {
                            ink_dirty.add(dirty.x0, dirty.y0, 0);
                            ink_dirty.add(dirty.x1, dirty.y1, 0);
                        }
                        *last_pen = Some(Instant::now());
                    } else if let State::Reading {
                        ref pages, page, ..
                    } = state
                    {
                        state = State::Fading {
                            stage: 0,
                            next: Instant::now(),
                            region: pages[page].region,
                        };
                    }
                }
                qtfb::INPUT_PEN_RELEASE => {
                    stylus_on = false;
                    if pen_down {
                        pen_down = false;
                        user_ink.pen_up();
                        if let State::Listening { ref mut last_pen } = state {
                            *last_pen = Some(Instant::now());
                        }
                    }
                }
                _ => {}
            }
        }

        if new_chat {
            eprintln!("carlito: new chat (two-finger swipe down)");
            history.clear();
            user_ink.clear();
            ink_dirty = BBox::empty();
            pen_down = false;
            page_tap = None;
            surf.fill_rect(0, 0, surf.w, surf.h, WHITE);
            disp.full_refresh(surf.w, surf.h);
            // Drop an in-flight receiver so an old reply cannot enter the new chat.
            state = State::Listening { last_pen: None };
        }

        if !ink_dirty.is_empty() && last_flush.elapsed() >= flush_every {
            let (x, y, w, h) = ink_dirty.rect_clamped(surf.w, surf.h);
            disp.update(x, y, w, h, true);
            ink_dirty = BBox::empty();
            last_flush = Instant::now();
        }

        state = match state {
            State::Listening { last_pen } => match last_pen {
                Some(last)
                    if !pen_down && last.elapsed() >= IDLE_COMMIT && !user_ink.is_empty() =>
                {
                    if help::looks_like_question_mark(user_ink.stroke_list()) {
                        let (x, y, w, h) = user_ink.bbox.rect_clamped(surf.w, surf.h);
                        surf.fill_rect(x as usize, y as usize, w as usize, h as usize, WHITE);
                        disp.update(x, y, w, h, false);
                        user_ink.clear();
                        let panel = help::show(&mut surf, &font);
                        let (x, y, w, h) = panel.region.rect_clamped(surf.w, surf.h);
                        disp.update(x, y, w, h, false);
                        State::Help {
                            panel: Some(panel),
                            until: Instant::now() + Duration::from_secs(45),
                        }
                    } else {
                        let captured = user_ink.to_png(&surf, PNG_PATH);
                        let (tx, rx) = mpsc::channel();
                        match captured {
                            Ok(()) => {
                                if let Some(ref oracle) = oracle {
                                    oracle.ask_with_history(PNG_PATH, &history, tx);
                                } else {
                                    let _ = tx.send(Ok(
                                        "I can't start my answer engine. Check Carlito's settings, then try again."
                                            .into(),
                                    ));
                                }
                            }
                            Err(e) => {
                                eprintln!("carlito: could not rasterize handwriting: {e}");
                                let _ = tx.send(Ok(
                                    "I couldn't capture that question. Please write it once more."
                                        .into(),
                                ));
                            }
                        }
                        State::Absorbing {
                            stage: 0,
                            next: Instant::now(),
                            region: user_ink.bbox,
                            rx,
                        }
                    }
                }
                _ => State::Listening { last_pen },
            },

            State::Absorbing {
                stage,
                next,
                region,
                rx,
            } => {
                const STAGES: u32 = 12;
                if Instant::now() >= next {
                    ink::dissolve_pass(&mut surf, region, stage, STAGES);
                    let (x, y, w, h) = region.rect_clamped(surf.w, surf.h);
                    disp.update(x, y, w, h, true);
                    if stage + 1 >= STAGES {
                        user_ink.clear();
                        State::Thinking {
                            rx,
                            pulse: Instant::now(),
                            blot_on: false,
                            answer: String::new(),
                            failed: false,
                        }
                    } else {
                        State::Absorbing {
                            stage: stage + 1,
                            next: Instant::now() + Duration::from_millis(55),
                            region,
                            rx,
                        }
                    }
                } else {
                    State::Absorbing {
                        stage,
                        next,
                        region,
                        rx,
                    }
                }
            }

            State::Thinking {
                rx,
                pulse,
                blot_on,
                mut answer,
                mut failed,
            } => match rx.try_recv() {
                Ok(Ok(chunk)) => {
                    append_chunk(&mut answer, &chunk);
                    State::Thinking {
                        rx,
                        pulse,
                        blot_on,
                        answer,
                        failed,
                    }
                }
                Ok(Err(e)) => {
                    eprintln!("carlito: answer failed: {e}");
                    failed = true;
                    State::Thinking {
                        rx,
                        pulse,
                        blot_on,
                        answer,
                        failed,
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if pulse.elapsed() >= Duration::from_millis(350) {
                        let (cx, cy) = (surf.w as i32 / 2, surf.h as i32 / 2);
                        if blot_on {
                            surf.fill_rect(cx as usize - 12, cy as usize - 12, 24, 24, WHITE);
                        } else {
                            draw_bolt(&mut surf, cx, cy, 22);
                        }
                        disp.update(cx - 14, cy - 14, 28, 28, true);
                        State::Thinking {
                            rx,
                            pulse: Instant::now(),
                            blot_on: !blot_on,
                            answer,
                            failed,
                        }
                    } else {
                        State::Thinking {
                            rx,
                            pulse,
                            blot_on,
                            answer,
                            failed,
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    let (cx, cy) = (surf.w / 2, surf.h / 2);
                    surf.fill_rect(cx.saturating_sub(16), cy.saturating_sub(16), 32, 32, WHITE);
                    if answer.trim().is_empty() {
                        answer = if failed {
                            "That answer got lost on the way. Please ask me once more.".into()
                        } else {
                            "I couldn't make out the question. Write it a little larger and try me again."
                                .into()
                        };
                    }
                    // Remember for in-session memory (cleared on two-finger or exit)
                    history.push(answer.clone());
                    if history.len() > 8 {
                        history.remove(0);
                    }
                    let blocks = parse_blocks(&answer);
                    let pages = paginate_blocks(&font, &blocks, surf.w, surf.h);
                    prepare_page(&font, &mut surf, &disp, 0, pages.len());
                    State::Writing {
                        pages,
                        page: 0,
                        stroke_i: 0,
                        point_i: 0,
                        next: Instant::now(),
                    }
                }
            },

            State::Writing {
                pages,
                page,
                mut stroke_i,
                mut point_i,
                next,
            } => {
                if Instant::now() >= next {
                    let mut dirty = BBox::empty();
                    let mut budget = 110;
                    while budget > 0 && stroke_i < pages[page].strokes.len() {
                        let stroke = &pages[page].strokes[stroke_i];
                        if point_i >= stroke.len() {
                            stroke_i += 1;
                            point_i = 0;
                            continue;
                        }
                        let (x, y) = stroke[point_i];
                        if point_i > 0 {
                            let (px, py) = stroke[point_i - 1];
                            surf.brush_line(px, py, x, y, REPLY_INK_R, BLACK);
                        } else {
                            surf.stamp(x, y, REPLY_INK_R, BLACK);
                        }
                        dirty.add(x, y, 4);
                        point_i += 1;
                        budget -= 1;
                    }
                    if !dirty.is_empty() {
                        let (x, y, w, h) = dirty.rect_clamped(surf.w, surf.h);
                        disp.update(x, y, w, h, true);
                    }
                    if stroke_i >= pages[page].strokes.len() {
                        let reading_time = if pages.len() > 1 {
                            Duration::from_secs(120)
                        } else {
                            Duration::from_secs(45)
                        };
                        State::Reading {
                            pages,
                            page,
                            until: Instant::now() + reading_time,
                        }
                    } else {
                        State::Writing {
                            pages,
                            page,
                            stroke_i,
                            point_i,
                            next: Instant::now() + Duration::from_millis(6),
                        }
                    }
                } else {
                    State::Writing {
                        pages,
                        page,
                        stroke_i,
                        point_i,
                        next,
                    }
                }
            }

            State::Reading {
                pages,
                page,
                mut until,
            } => {
                let target = page_tap.and_then(|(x, _)| {
                    if x < surf.w as i32 / 2 && page > 0 {
                        Some(page - 1)
                    } else if x >= surf.w as i32 / 2 && page + 1 < pages.len() {
                        Some(page + 1)
                    } else {
                        None
                    }
                });
                if let Some(target) = target {
                    prepare_page(&font, &mut surf, &disp, target, pages.len());
                    State::Writing {
                        pages,
                        page: target,
                        stroke_i: 0,
                        point_i: 0,
                        next: Instant::now(),
                    }
                } else if Instant::now() >= until {
                    State::Fading {
                        stage: 0,
                        next: Instant::now(),
                        region: pages[page].region,
                    }
                } else {
                    if page_tap.is_some() {
                        until = Instant::now() + Duration::from_secs(120);
                    }
                    State::Reading { pages, page, until }
                }
            }

            State::Help { panel, until } => match panel {
                Some(panel) => {
                    if stylus_tapped || Instant::now() >= until {
                        let region = panel.dismiss(&mut surf);
                        let (x, y, w, h) = region.rect_clamped(surf.w, surf.h);
                        disp.update(x, y, w, h, false);
                        State::Help { panel: None, until }
                    } else {
                        State::Help {
                            panel: Some(panel),
                            until,
                        }
                    }
                }
                None if stylus_on => State::Help { panel: None, until },
                None => State::Listening { last_pen: None },
            },

            State::Fading {
                stage,
                next,
                region,
            } => {
                const STAGES: u32 = 8;
                if Instant::now() >= next {
                    ink::dissolve_pass(&mut surf, region, stage, STAGES);
                    let (x, y, w, h) = region.rect_clamped(surf.w, surf.h);
                    disp.update(x, y, w, h, true);
                    if stage + 1 >= STAGES {
                        surf.fill_rect(0, 0, surf.w, surf.h, WHITE);
                        disp.full_refresh(surf.w, surf.h);
                        State::Listening { last_pen: None }
                    } else {
                        State::Fading {
                            stage: stage + 1,
                            next: Instant::now() + Duration::from_millis(60),
                            region,
                        }
                    }
                } else {
                    State::Fading {
                        stage,
                        next,
                        region,
                    }
                }
            }
        };

        stylus_tapped = false;
        std::thread::sleep(Duration::from_millis(2));
    }

    eprintln!("carlito: closed");
    disp.terminate();
    Ok(())
}

fn sleep_and_restore(
    button: &mut power::PowerButton,
    disp: &display::Display,
    surf: &mut Surface,
    font: &FontRef,
    pen_dev: &mut Option<pen::PenDevice>,
    touch_dev: &mut Option<touch::TouchDevice>,
) -> Duration {
    let sleep_started = Instant::now();
    eprintln!("carlito: sleeping");
    let saved = help::show_sleep(surf, font);
    disp.full_refresh(surf.w, surf.h);
    std::thread::sleep(Duration::from_millis(800));
    let count = power::suspend_count();
    for _ in 0..8 {
        if button.grabbed {
            let _ = std::process::Command::new("systemctl")
                .arg("suspend")
                .status();
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(6) {
            std::thread::sleep(Duration::from_millis(400));
            if power::suspend_count() > count {
                break;
            }
        }
        if power::suspend_count() > count {
            break;
        }
    }
    help::restore_sleep(surf, &saved);
    disp.full_refresh(surf.w, surf.h);
    power::wifi_heal();
    if let Some(pen) = pen_dev {
        let _ = pen.drain();
    }
    if let Some(touch) = touch_dev {
        let _ = touch.drain();
    }
    button.drain_pressed();
    sleep_started.elapsed()
}

fn append_chunk(answer: &mut String, chunk: &str) {
    let chunk = chunk.trim();
    if chunk.is_empty() {
        return;
    }
    if !answer.is_empty()
        && !answer.ends_with(char::is_whitespace)
        && !chunk.starts_with(char::is_whitespace)
    {
        answer.push(' ');
    }
    answer.push_str(chunk);
}

fn paginate(font: &FontRef, text: &str, screen_w: usize, screen_h: usize) -> Vec<Page> {
    let margin_x = (screen_w / 11).clamp(48, 140) as i32;
    let px = (screen_w as f32 / 22.5).clamp(52.0, 78.0);
    let line_h = (px * 1.32) as i32;
    let top = (screen_h as i32 / 10).max(90);
    let bottom = screen_h as i32 - (screen_h as i32 / 10).max(100);
    let content_h = (bottom - top).max(line_h);
    let lines_per_page = (content_h / line_h).max(1) as usize;
    let max_w = (screen_w as i32 - margin_x * 2).max(200) as f32;
    let mut lines = wrap_safe(font, text, px, max_w);
    if lines.is_empty() {
        lines.push("I'm here. Ask me anything.".into());
    }

    let mut pages = Vec::new();
    for page_lines in lines.chunks(lines_per_page) {
        let total_h = line_h * page_lines.len() as i32;
        let mut y = top + (content_h - total_h).max(0) / 6;
        let mut strokes = Vec::new();
        let mut region = BBox::empty();
        for line_text in page_lines {
            if !line_text.is_empty() {
                let mut raster = script::rasterize_line(font, line_text, px);
                script::thin(&mut raster);
                let max_x = (screen_w as i32 - margin_x - raster.width as i32).max(margin_x);
                let x0 = ((screen_w as i32 - raster.width as i32) / 2).clamp(margin_x, max_x);
                for stroke in script::trace(&raster) {
                    let mapped: Vec<(i32, i32)> =
                        stroke.iter().map(|&(x, sy)| (x0 + x, y + sy)).collect();
                    for &(x, sy) in &mapped {
                        region.add(x, sy, 5);
                    }
                    strokes.push(mapped);
                }
            }
            y += line_h;
        }
        if region.is_empty() {
            region.add(screen_w as i32 / 2, screen_h as i32 / 2, 1);
        }
        pages.push(Page { strokes, region });
    }
    pages
}

fn wrap_safe(font: &FontRef, text: &str, px: f32, max_w: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        if paragraph.trim().is_empty() {
            if lines.last().is_some_and(|line: &String| !line.is_empty()) {
                lines.push(String::new());
            }
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let mut pieces = split_long_word(font, word, px, max_w);
            for piece in pieces.drain(..) {
                let candidate = if current.is_empty() {
                    piece.clone()
                } else {
                    format!("{current} {piece}")
                };
                if script::measure(font, &candidate, px) <= max_w {
                    current = candidate;
                } else {
                    if !current.is_empty() {
                        lines.push(std::mem::take(&mut current));
                    }
                    current = piece;
                }
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

fn split_long_word(font: &FontRef, word: &str, px: f32, max_w: f32) -> Vec<String> {
    if script::measure(font, word, px) <= max_w {
        return vec![word.to_string()];
    }
    let mut pieces = Vec::new();
    let mut piece = String::new();
    for ch in word.chars() {
        let mut candidate = piece.clone();
        candidate.push(ch);
        if !piece.is_empty() && script::measure(font, &candidate, px) > max_w {
            pieces.push(std::mem::take(&mut piece));
        }
        piece.push(ch);
    }
    if !piece.is_empty() {
        pieces.push(piece);
    }
    pieces
}

fn prepare_page(
    font: &FontRef,
    surf: &mut Surface,
    disp: &display::Display,
    page: usize,
    total: usize,
) {
    surf.fill_rect(0, 0, surf.w, surf.h, WHITE);
    draw_mark(
        surf,
        surf.w as i32 / 2,
        (surf.h / 24).clamp(44, 78) as i32,
        28,
    );
    if total > 1 {
        let footer = format!("<     {} / {}     >", page + 1, total);
        let y = surf.h.saturating_sub((surf.h / 16).clamp(70, 120));
        blit_centered(surf, font, &footer, 38.0, y);
    }
    disp.update_all(surf.w, surf.h);
}

fn show_splash(font: &FontRef, surf: &mut Surface) {
    surf.fill_rect(0, 0, surf.w, surf.h, WHITE);
    let cy = surf.h as i32 * 35 / 100;
    draw_mark(surf, surf.w as i32 / 2, cy, 92);
    blit_centered(surf, font, "Carlito", 132.0, (cy + 180) as usize);
    blit_centered(
        surf,
        font,
        "quick answers, written quietly",
        48.0,
        (cy + 350) as usize,
    );
}

fn draw_mark(surf: &mut Surface, cx: i32, cy: i32, radius: i32) {
    let mut previous = None;
    for degrees in 45..=315 {
        let angle = degrees as f32 * std::f32::consts::PI / 180.0;
        let point = (
            cx + (radius as f32 * angle.cos()) as i32,
            cy + (radius as f32 * angle.sin()) as i32,
        );
        if let Some((px, py)) = previous {
            surf.brush_line(px, py, point.0, point.1, (radius / 30).max(1), BLACK);
        }
        previous = Some(point);
    }
    draw_bolt(surf, cx + radius / 5, cy, radius * 6 / 5);
}

fn draw_bolt(surf: &mut Surface, cx: i32, cy: i32, size: i32) {
    let points = [
        (cx + size / 5, cy - size / 2),
        (cx - size / 5, cy),
        (cx + size / 8, cy),
        (cx - size / 4, cy + size / 2),
    ];
    for pair in points.windows(2) {
        surf.brush_line(pair[0].0, pair[0].1, pair[1].0, pair[1].1, 2, BLACK);
    }
}

fn blit_centered(surf: &mut Surface, font: &FontRef, text: &str, px: f32, y: usize) {
    let raster = script::rasterize_line(font, text, px);
    let x = surf.w.saturating_sub(raster.width) / 2;
    for row in 0..raster.height {
        for col in 0..raster.width {
            if raster.mask[row * raster.width + col] {
                surf.put_px((x + col) as i32, (y + row) as i32, BLACK);
            }
        }
    }
}

fn parse_blocks(answer: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    for line in answer.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("DRAW:") {
            if !current.trim().is_empty() {
                blocks.push(Block::Text(current.trim().to_string()));
                current.clear();
            }
            let json = trimmed.strip_prefix("DRAW:").unwrap().trim();
            if let Some(draw) = parse_draw_json(json) {
                blocks.push(draw);
            } else {
                // Fallback: treat line as text if parse fails
                current.push_str(line);
                current.push('\n');
            }
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }
    if !current.trim().is_empty() {
        blocks.push(Block::Text(current.trim().to_string()));
    }
    if blocks.is_empty() {
        blocks.push(Block::Text("I'm here. Ask me anything.".into()));
    }
    blocks
}

fn parse_draw_json(s: &str) -> Option<Block> {
    // Expect {"lines":[[x1,y1,x2,y2],...],"caption":"..."}
    let lines_start = s.find("\"lines\"")?;
    let bracket = s[lines_start..].find('[')?;
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut end = 0usize;
    let chars: Vec<char> = s[lines_start + bracket..].chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '[' {
            if depth == 0 {
                start = i;
            }
            depth += 1;
        } else if *c == ']' {
            depth -= 1;
            if depth == 0 {
                end = i;
                break;
            }
        }
    }
    if end == 0 {
        return None;
    }
    let inner = chars[start + 1..end].iter().collect::<String>();
    // inner like "[0,0,1000,0],[0,0,0,600]" → split by "],["
    let mut lines = Vec::new();
    // Extract numbers via simple scan
    let mut nums: Vec<i32> = Vec::new();
    let mut num_buf = String::new();
    for ch in inner.chars().chain(std::iter::once(',')) {
        if ch.is_ascii_digit() || ch == '-' {
            num_buf.push(ch);
        } else if !num_buf.is_empty() {
            if let Ok(n) = num_buf.parse::<i32>() {
                nums.push(n.clamp(0, 1000));
            }
            num_buf.clear();
        }
    }
    for chunk in nums.chunks(4) {
        if chunk.len() == 4 {
            lines.push([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
    }
    if lines.is_empty() {
        return None;
    }
    let caption = if let Some(pos) = s.find("\"caption\"") {
        let after = &s[pos + 9..];
        if let Some(colon) = after.find(':') {
            let rest = after[colon + 1..].trim();
            if rest.starts_with('"') {
                let end = rest[1..].find('"').unwrap_or(0);
                rest[1..1 + end].to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    Some(Block::Draw { lines, caption })
}

fn paginate_blocks(
    font: &FontRef,
    blocks: &[Block],
    screen_w: usize,
    screen_h: usize,
) -> Vec<Page> {
    let margin_x = (screen_w / 11).clamp(48, 140) as i32;
    let px = (screen_w as f32 / 22.5).clamp(52.0, 78.0);
    let line_h = (px * 1.32) as i32;
    let top = (screen_h as i32 / 10).max(90);
    let bottom = screen_h as i32 - (screen_h as i32 / 10).max(100);
    let content_h = (bottom - top).max(line_h);
    let max_w = (screen_w as i32 - margin_x * 2).max(200) as f32;
    let draw_h = (content_h / 3).clamp(220, 420);

    let mut pages: Vec<Page> = Vec::new();
    let mut cur_page_strokes: Vec<Vec<(i32, i32)>> = Vec::new();
    let mut cur_region = BBox::empty();
    let mut cur_y = top;

    let flush_page = |strokes: &mut Vec<Vec<(i32, i32)>>,
                      region: &mut BBox,
                      y: &mut i32,
                      pages: &mut Vec<Page>| {
        if strokes.is_empty() {
            return;
        }
        if region.is_empty() {
            region.add(screen_w as i32 / 2, screen_h as i32 / 2, 1);
        }
        pages.push(Page {
            strokes: std::mem::take(strokes),
            region: *region,
        });
        *region = BBox::empty();
        *y = top;
    };

    for block in blocks {
        match block {
            Block::Text(text) => {
                let lines = wrap_safe(font, text, px, max_w);
                for line_text in lines {
                    if line_text.is_empty() {
                        cur_y += line_h / 2;
                        if cur_y + line_h > bottom {
                            flush_page(
                                &mut cur_page_strokes,
                                &mut cur_region,
                                &mut cur_y,
                                &mut pages,
                            );
                        }
                        continue;
                    }
                    if cur_y + line_h > bottom {
                        flush_page(
                            &mut cur_page_strokes,
                            &mut cur_region,
                            &mut cur_y,
                            &mut pages,
                        );
                    }
                    let mut raster = script::rasterize_line(font, &line_text, px);
                    script::thin(&mut raster);
                    let max_x = (screen_w as i32 - margin_x - raster.width as i32).max(margin_x);
                    let x0 = ((screen_w as i32 - raster.width as i32) / 2).clamp(margin_x, max_x);
                    for stroke in script::trace(&raster) {
                        let mapped: Vec<(i32, i32)> =
                            stroke.iter().map(|&(x, sy)| (x0 + x, cur_y + sy)).collect();
                        for &(x, sy) in &mapped {
                            cur_region.add(x, sy, 5);
                        }
                        cur_page_strokes.push(mapped);
                    }
                    cur_y += line_h;
                }
            }
            Block::Draw { lines, caption } => {
                // Reserve space: draw_h + caption
                let needed = draw_h + if caption.is_empty() { 0 } else { line_h } + 10;
                if cur_y + needed > bottom && !cur_page_strokes.is_empty() {
                    flush_page(
                        &mut cur_page_strokes,
                        &mut cur_region,
                        &mut cur_y,
                        &mut pages,
                    );
                }
                let y0 = cur_y;
                for seg in lines {
                    let x1 = margin_x + (seg[0] as f32 / 1000.0 * max_w as f32) as i32;
                    let y1 = y0 + (seg[1].clamp(0, 600) as f32 / 600.0 * draw_h as f32) as i32;
                    let x2 = margin_x + (seg[2] as f32 / 1000.0 * max_w as f32) as i32;
                    let y2 = y0 + (seg[3].clamp(0, 600) as f32 / 600.0 * draw_h as f32) as i32;
                    // Single stroke for the segment
                    cur_page_strokes.push(vec![(x1, y1), (x2, y2)]);
                    cur_region.add(x1, y1, 2);
                    cur_region.add(x2, y2, 2);
                }
                // Border around draw area
                cur_region.add(margin_x, y0, 2);
                cur_region.add(margin_x + max_w as i32, y0 + draw_h, 2);
                cur_y += draw_h + 6;
                if !caption.is_empty() {
                    // Caption centered below drawing, smaller px
                    let cap_px = (px * 0.62).clamp(32.0, 48.0);
                    let cap_lines = wrap_safe(font, caption, cap_px, max_w);
                    for cap in cap_lines {
                        if cur_y + (cap_px * 1.2) as i32 > bottom {
                            flush_page(
                                &mut cur_page_strokes,
                                &mut cur_region,
                                &mut cur_y,
                                &mut pages,
                            );
                        }
                        let mut raster = script::rasterize_line(font, &cap, cap_px);
                        script::thin(&mut raster);
                        let max_x =
                            (screen_w as i32 - margin_x - raster.width as i32).max(margin_x);
                        let x0 =
                            ((screen_w as i32 - raster.width as i32) / 2).clamp(margin_x, max_x);
                        for stroke in script::trace(&raster) {
                            let mapped: Vec<(i32, i32)> =
                                stroke.iter().map(|&(x, sy)| (x0 + x, cur_y + sy)).collect();
                            for &(x, sy) in &mapped {
                                cur_region.add(x, sy, 3);
                            }
                            cur_page_strokes.push(mapped);
                        }
                        cur_y += (cap_px * 1.2) as i32;
                    }
                }
                cur_y += line_h / 2;
            }
        }
    }
    if !cur_page_strokes.is_empty() {
        flush_page(
            &mut cur_page_strokes,
            &mut cur_region,
            &mut cur_y,
            &mut pages,
        );
    }
    if pages.is_empty() {
        let mut region = BBox::empty();
        region.add(screen_w as i32 / 2, screen_h as i32 / 2, 1);
        pages.push(Page {
            strokes: Vec::new(),
            region,
        });
    }
    pages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_answers_are_paginated_inside_the_screen() {
        let font = FontRef::try_from_slice(FONT_TTF).unwrap();
        let text =
            "Carlito gives useful answers without allowing them to leave the frame. ".repeat(120);
        let pages = paginate(&font, &text, 954, 1696);
        assert!(pages.len() > 2);
        for page in pages {
            assert!(page.region.x0 >= 0, "left overflow: {:?}", page.region);
            assert!(page.region.y0 >= 0, "top overflow: {:?}", page.region);
            assert!(page.region.x1 < 954, "right overflow: {:?}", page.region);
            assert!(page.region.y1 < 1696, "bottom overflow: {:?}", page.region);
        }
    }

    #[test]
    fn long_unbroken_text_is_split() {
        let font = FontRef::try_from_slice(FONT_TTF).unwrap();
        let lines = wrap_safe(&font, &"x".repeat(500), 52.0, 700.0);
        assert!(lines.len() > 1);
        assert!(lines
            .iter()
            .all(|line| script::measure(&font, line, 52.0) <= 700.0));
    }
}
