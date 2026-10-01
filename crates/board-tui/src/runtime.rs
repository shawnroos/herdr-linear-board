//! Terminal setup, the clock, and the draw/input loop.
//!
//! The only module that touches a real terminal or the wall clock; everything
//! it drives ([`Driver`], `app::update`, `view::view`) stays testable without
//! either.

use std::io::Stdout;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use board_core::client::{BoardClient, UnixClient};
use board_core::protocol::{BoardSnapshot, Event};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event as CtEvent, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::app::{LocalStateSignals, Msg};
use crate::driver::LinearStart;
use crate::editor::RealEditor;
use crate::view::view;
use crate::{Driver, OriginContext};

const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(100);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
enum SubscriptionSignal {
    Changed,
    LocalState {
        space: Option<String>,
        snapshot: bool,
    },
    Reconnected,
}

/// Every signal drained in one idle tick, merged.
#[derive(Debug, Default, PartialEq, Eq)]
struct Coalesced {
    changed: bool,
    reconnected: bool,
    local: LocalStateSignals,
}

fn coalesce(signals: impl IntoIterator<Item = SubscriptionSignal>) -> Coalesced {
    let mut out = Coalesced::default();
    for signal in signals {
        match signal {
            SubscriptionSignal::Changed => out.changed = true,
            SubscriptionSignal::Reconnected => out.reconnected = true,
            SubscriptionSignal::LocalState { space, snapshot } => out.local.add(space, snapshot),
        }
    }
    out
}

fn epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// The `board tui` entry point: set up the terminal, spawn an event-subscription
/// thread, and run the draw/input loop until quit.
pub fn run(client: Box<dyn BoardClient>) -> Result<()> {
    let mut driver = Driver::new(client)?;
    run_driver(&mut driver)
}

pub fn run_with_board(client: Box<dyn BoardClient>, board: BoardSnapshot) -> Result<()> {
    let mut driver = Driver::with_editor_and_board_and_origin(
        client,
        Box::new(RealEditor),
        board,
        OriginContext::from_environment(),
    )?;
    run_driver(&mut driver)
}

/// The `board tui` entry point in Linear mode: no board row is read or
/// written; the first `linear.snapshot` leaves from the driver constructor.
pub fn run_linear(client: Box<dyn BoardClient>, start: LinearStart) -> Result<()> {
    let mut driver = Driver::linear(client, Box::new(RealEditor), start);
    run_driver(&mut driver)
}

/// The session side pane beside one agent: reads only, never a
/// write, not even its own pane title.
pub fn run_session(
    client: Box<dyn BoardClient>,
    start: LinearStart,
    identity: crate::SessionIdentity,
) -> Result<()> {
    let mut driver = Driver::session(client, Box::new(crate::RealPlatform), start, identity);
    run_driver(&mut driver)
}

fn run_driver(driver: &mut Driver) -> Result<()> {
    // Live updates use a dedicated socket. A Unix client also supplies the
    // exact path needed to recover after boardd replacement; embedded/fake
    // clients keep the old one-shot/action-driven fallback.
    let (tx, rx) = mpsc::channel::<SubscriptionSignal>();
    let initial_stream = driver.subscribe().ok();
    match driver.reconnect_path() {
        Some(path) => {
            std::thread::spawn(move || {
                supervise_subscription(
                    initial_stream,
                    move || {
                        let mut client = UnixClient::connect(&path)?;
                        client.subscribe()
                    },
                    tx,
                    std::thread::sleep,
                );
            });
        }
        None => {
            if let Some(stream) = initial_stream {
                std::thread::spawn(move || forward_events(stream, &tx));
            }
        }
    }

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(driver, &mut terminal, &rx);

    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    res
}

fn event_loop(
    driver: &mut Driver,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    rx: &mpsc::Receiver<SubscriptionSignal>,
) -> Result<()> {
    loop {
        driver.app.now = epoch_secs();
        driver.app.now_ms = epoch_millis();
        driver.expire_toast();
        driver.drain_linear_arrivals();

        let size = terminal.size()?;
        let new_area = Rect::new(0, 0, size.width, size.height);
        // Bug A (resize half): a size change can leave stale cells behind too
        // (e.g. shrinking then growing back); force a full repaint whenever
        // the terminal size differs from what the last frame used.
        driver.sync_frame_area(new_area);

        if driver.take_needs_full_redraw() {
            terminal.clear()?;
        }
        terminal.draw(|f| view(&driver.app, f))?;

        if crossterm::event::poll(Duration::from_millis(200))? {
            match crossterm::event::read()? {
                CtEvent::Key(k) if k.kind == KeyEventKind::Press => {
                    driver.handle(Msg::Key(k));
                }
                CtEvent::Mouse(m) => driver.handle(Msg::Mouse(m)),
                _ => {}
            }
        } else {
            // Drain and coalesce pending refresh/reconnect signals. A daemon
            // replacement invalidates the request socket as well as the event
            // stream, so install a fresh request client before refetching;
            // the local-state read goes after it for the same reason.
            let batch = coalesce(std::iter::from_fn(|| rx.try_recv().ok()));
            driver.on_daemon_signals(batch.changed, batch.reconnected);
            if !batch.local.is_empty() {
                driver.on_local_state_changed(batch.local);
            }
        }

        if driver.app.should_quit {
            return Ok(());
        }
    }
}

fn forward_events(
    stream: Box<dyn Iterator<Item = Event> + Send>,
    tx: &mpsc::Sender<SubscriptionSignal>,
) {
    for event in stream {
        let signal = match event {
            Event::LocalStateChanged { space, snapshot } => {
                SubscriptionSignal::LocalState { space, snapshot }
            }
            _ => SubscriptionSignal::Changed,
        };
        if tx.send(signal).is_err() {
            break;
        }
    }
}

fn supervise_subscription<Reconnect, Sleep>(
    mut stream: Option<Box<dyn Iterator<Item = Event> + Send>>,
    reconnect: Reconnect,
    tx: mpsc::Sender<SubscriptionSignal>,
    sleep: Sleep,
) where
    Reconnect: Fn() -> Result<Box<dyn Iterator<Item = Event> + Send>>,
    Sleep: Fn(Duration),
{
    let mut backoff = RECONNECT_BACKOFF_MIN;
    loop {
        if let Some(current) = stream.take() {
            forward_events(current, &tx);
        }

        match reconnect() {
            Ok(next) => {
                // This ping forces a complete snapshot fetch even if every
                // event from the outage window was lost before resubscribe.
                if tx.send(SubscriptionSignal::Reconnected).is_err() {
                    return;
                }
                stream = Some(next);
                backoff = RECONNECT_BACKOFF_MIN;
            }
            Err(_) => {
                sleep(backoff);
                backoff = backoff.saturating_mul(2).min(RECONNECT_BACKOFF_MAX);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::protocol::{BoardChangedReason, Event};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    fn changed_event() -> Event {
        Event::BoardChanged {
            reason: BoardChangedReason::CardUpdated,
            board_id: Some(2),
            card_id: Some(7),
            column_id: Some(3),
        }
    }

    fn local(space: Option<&str>, snapshot: bool) -> SubscriptionSignal {
        SubscriptionSignal::LocalState {
            space: space.map(str::to_string),
            snapshot,
        }
    }

    #[test]
    fn a_local_state_event_is_forwarded_with_its_space_and_flag_not_as_a_board_change() {
        let (tx, rx) = mpsc::channel();
        let events = vec![
            Event::LocalStateChanged {
                space: Some("wA".into()),
                snapshot: true,
            },
            changed_event(),
            Event::LocalStateChanged {
                space: None,
                snapshot: false,
            },
        ];
        forward_events(Box::new(events.into_iter()), &tx);
        drop(tx);
        assert_eq!(
            rx.iter().collect::<Vec<_>>(),
            vec![
                local(Some("wA"), true),
                SubscriptionSignal::Changed,
                local(None, false)
            ]
        );
    }

    #[test]
    fn the_coalescer_keeps_each_space_once_and_whether_any_of_its_events_carried_the_flag() {
        let batch = coalesce([
            local(Some("wA"), false),
            local(Some("wA"), false),
            local(Some("wA"), false),
            local(Some("wB"), true),
            local(Some("wB"), false),
        ]);
        assert!(!batch.changed && !batch.reconnected);
        let mut expected = LocalStateSignals::default();
        expected.add(Some("wA".into()), false);
        expected.add(Some("wB".into()), true);
        assert_eq!(batch.local, expected);
        assert_eq!(batch.local.for_space("wA"), Some(false));
        assert_eq!(batch.local.for_space("wB"), Some(true));
        assert_eq!(batch.local.for_space("wC"), None);

        let batch = coalesce([
            SubscriptionSignal::Changed,
            local(None, false),
            SubscriptionSignal::Reconnected,
        ]);
        assert!(batch.changed && batch.reconnected);
        assert_eq!(
            batch.local.for_space("wC"),
            Some(false),
            "an any-space event concerns every space"
        );
        assert!(coalesce([]).local.is_empty());
    }

    #[test]
    fn subscription_retries_then_forces_refetch_before_forwarding_events() {
        let (signal_tx, signal_rx) = mpsc::channel();
        let attempts = Arc::new(Mutex::new(0_u8));
        let attempts_in_reconnect = attempts.clone();
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let sleeps_in_fn = sleeps.clone();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));

        struct HeldStream {
            events: VecDeque<Event>,
            release: Arc<Mutex<mpsc::Receiver<()>>>,
        }

        impl Iterator for HeldStream {
            type Item = Event;

            fn next(&mut self) -> Option<Event> {
                self.events
                    .pop_front()
                    .or_else(|| self.release.lock().ok()?.recv().ok().and(None))
            }
        }

        let worker = std::thread::spawn(move || {
            supervise_subscription(
                Some(Box::new(std::iter::empty())),
                move || {
                    let mut attempts = attempts_in_reconnect.lock().unwrap();
                    *attempts += 1;
                    if *attempts == 1 {
                        anyhow::bail!("daemon absent")
                    }
                    Ok(Box::new(HeldStream {
                        events: VecDeque::from([changed_event()]),
                        release: release_rx.clone(),
                    })
                        as Box<dyn Iterator<Item = Event> + Send>)
                },
                signal_tx,
                move |delay| sleeps_in_fn.lock().unwrap().push(delay),
            );
        });

        assert_eq!(signal_rx.recv().unwrap(), SubscriptionSignal::Reconnected);
        assert_eq!(signal_rx.recv().unwrap(), SubscriptionSignal::Changed);
        assert_eq!(*sleeps.lock().unwrap(), vec![RECONNECT_BACKOFF_MIN]);

        drop(signal_rx);
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(*attempts.lock().unwrap(), 3);
    }
}
