//! Closes Claude Code and Codex sessions that sat idle for the time set in
//! Settings, to free their memory. The last minutes count down in the
//! sidebar; selecting the session later continues its conversation.

use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use gtk::glib;
use uuid::Uuid;

use crate::app::App;
use crate::model::SessionProvider;
use crate::runtime::TabRuntime;
use crate::term::status::State;

/// The countdown shows for this long before a session closes.
const SHOW_COUNTDOWN: u64 = 5 * 60;

/// Seconds until the session closes, or None if it isn't counting down:
/// only agent sessions that run, aren't working or waiting for the user
/// (nor any of their subagents), and aren't the selected one.
pub fn remaining(rt: &TabRuntime, selected: bool, idle: u64, limit: u64) -> Option<u64> {
    if limit == 0 || selected || rt.exited {
        return None;
    }
    if !matches!(
        rt.provider,
        Some(SessionProvider::Claude | SessionProvider::Codex)
    ) {
        return None;
    }
    let busy = |s: State| matches!(s, State::Working | State::Blocked);
    // Subagents and background work count too.
    if rt.status_state.is_some_and(busy) || rt.status_children.iter().any(|r| busy(r.state)) {
        return None;
    }
    Some(limit.saturating_sub(idle))
}

/// Seconds since the user typed into the session or its status changed.
fn idle_seconds(last_input: Instant, touched: Option<SystemTime>) -> u64 {
    let typed = last_input.elapsed();
    let changed = touched
        .and_then(|t| t.elapsed().ok())
        .unwrap_or(Duration::MAX);
    typed.min(changed).as_secs()
}

fn tick(app: &Rc<App>) {
    let limit = u64::from(app.settings.borrow().idle_close_minutes) * 60;
    let terminals: Vec<(Uuid, Instant)> = app
        .terminals
        .borrow()
        .iter()
        .map(|(id, term)| (*id, term.last_input.get()))
        .collect();
    for (id, last_input) in terminals {
        let rt = app.rt(id);
        let selected = app.selected.get() == Some(id);
        let left = remaining(&rt, selected, idle_seconds(last_input, rt.touched), limit);
        if left == Some(0) {
            app.close_idle(id);
            continue;
        }
        let shown = left.filter(|s| *s <= SHOW_COUNTDOWN);
        if rt.closes_in != shown {
            app.update_rt(id, |rt| rt.closes_in = shown);
            app.refresh_session(id);
        }
    }
}

pub fn install(app: &Rc<App>) {
    let weak = Rc::downgrade(app);
    glib::timeout_add_local(Duration::from_secs(1), move || {
        let Some(app) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        tick(&app);
        glib::ControlFlow::Continue
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude(state: Option<State>) -> TabRuntime {
        TabRuntime {
            provider: Some(SessionProvider::Claude),
            status_state: state,
            ..TabRuntime::default()
        }
    }

    #[test]
    fn who_counts_down() {
        let idle = claude(Some(State::Done));
        assert_eq!(remaining(&idle, false, 100, 900), Some(800));
        assert_eq!(remaining(&idle, false, 1000, 900), Some(0));
        assert_eq!(remaining(&claude(None), false, 0, 900), Some(900));
        // Off, selected, working, waiting for the user, stopped, a shell.
        assert_eq!(remaining(&idle, false, 100, 0), None);
        assert_eq!(remaining(&idle, true, 100, 900), None);
        assert_eq!(
            remaining(&claude(Some(State::Working)), false, 9999, 900),
            None
        );
        assert_eq!(
            remaining(&claude(Some(State::Blocked)), false, 9999, 900),
            None
        );
        let stopped = TabRuntime {
            exited: true,
            ..claude(None)
        };
        assert_eq!(remaining(&stopped, false, 9999, 900), None);
        let shell = TabRuntime {
            provider: Some(SessionProvider::Shell),
            ..TabRuntime::default()
        };
        assert_eq!(remaining(&shell, false, 9999, 900), None);
    }

    #[test]
    fn idle_is_the_latest_of_typing_and_status() {
        let now = Instant::now();
        assert_eq!(idle_seconds(now, None), 0);
        let old = SystemTime::now() - Duration::from_secs(600);
        assert_eq!(idle_seconds(now, Some(old)), 0);
    }
}
