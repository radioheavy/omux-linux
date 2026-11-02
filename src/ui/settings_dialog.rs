//! Settings (Ctrl+,): the Mac's single settings form as an Adwaita
//! preferences dialog.

use std::rc::Rc;

use adw::prelude::*;

use crate::app::App;
use crate::i18n::tr;
use crate::notify::{play_sound, SOUNDS};
use crate::theme::THEMES;

const USAGE_INTERVALS: [(f64, &str); 4] = [
    (60.0, "1 minute"),
    (300.0, "5 minutes"),
    (1800.0, "30 minutes"),
    (3600.0, "Hour"),
];

fn combo(title: &str, labels: &[&str], selected: usize) -> adw::ComboRow {
    let row = adw::ComboRow::new();
    row.set_title(tr(title));
    let labels: Vec<&str> = labels.iter().map(|l| tr(l)).collect();
    row.set_model(Some(&gtk::StringList::new(&labels)));
    row.set_selected(selected as u32);
    row
}

fn switch(title: &str, subtitle: Option<&str>, active: bool) -> adw::SwitchRow {
    let row = adw::SwitchRow::new();
    row.set_title(tr(title));
    if let Some(subtitle) = subtitle {
        row.set_subtitle(tr(subtitle));
    }
    row.set_active(active);
    row
}

/// Calls `change` with the app whenever the switch flips.
fn on_switch(app: &Rc<App>, row: &adw::SwitchRow, change: impl Fn(&Rc<App>, bool) + 'static) {
    let weak = Rc::downgrade(app);
    row.connect_active_notify(move |row| {
        if let Some(app) = weak.upgrade() {
            change(&app, row.is_active());
        }
    });
}

pub fn present(app: &Rc<App>) {
    let settings = app.settings.borrow().clone();
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title(tr("Settings"));
    let page = adw::PreferencesPage::new();

    // Appearance
    let appearance = adw::PreferencesGroup::new();
    appearance.set_title(tr("Appearance"));
    let labels: Vec<&str> = THEMES.iter().map(|t| t.label).collect();
    let index = THEMES
        .iter()
        .position(|t| t.raw == settings.theme)
        .unwrap_or(1);
    let theme = combo("Theme", &labels, index);
    let weak = Rc::downgrade(app);
    theme.connect_selected_notify(move |row| {
        let Some(app) = weak.upgrade() else { return };
        let raw = THEMES[row.selected() as usize].raw.to_string();
        app.update_settings(|s| s.theme = raw);
        app.apply_theme();
    });
    appearance.add(&theme);

    const LANGUAGES: [(&str, &str); 3] = [
        ("auto", "System Language"),
        ("en", "English"),
        ("tr", "Türkçe"),
    ];
    let labels: Vec<&str> = LANGUAGES.iter().map(|(_, l)| *l).collect();
    let index = LANGUAGES
        .iter()
        .position(|(raw, _)| *raw == settings.language)
        .unwrap_or(0);
    let language = combo("Language", &labels, index);
    language.set_subtitle(tr("Applies the next time omux opens."));
    let weak = Rc::downgrade(app);
    language.connect_selected_notify(move |row| {
        let Some(app) = weak.upgrade() else { return };
        let raw = LANGUAGES[row.selected() as usize].0.to_string();
        app.update_settings(|s| s.language = raw);
    });
    appearance.add(&language);

    let mut families = vec!["System Mono".to_string()];
    families.extend(crate::ui::terminal::monospace_families(&app.ui.window));
    let family_labels: Vec<&str> = families.iter().map(String::as_str).collect();
    let index = families
        .iter()
        .position(|f| *f == settings.font_family)
        .unwrap_or(0);
    let font = combo("Font", &family_labels, index);
    let weak = Rc::downgrade(app);
    font.connect_selected_notify(move |row| {
        let Some(app) = weak.upgrade() else { return };
        let family = match row.selected() {
            0 => String::new(),
            i => families[i as usize].clone(),
        };
        app.update_settings(|s| s.font_family = family);
        app.apply_theme();
    });
    appearance.add(&font);

    let size = adw::SpinRow::with_range(9.0, 24.0, 1.0);
    size.set_title(tr("Font size"));
    size.set_value(settings.terminal_font_size());
    let weak = Rc::downgrade(app);
    size.connect_value_notify(move |row| {
        let Some(app) = weak.upgrade() else { return };
        let value = row.value();
        app.update_settings(|s| s.font_size = value);
        app.apply_theme();
    });
    appearance.add(&size);
    page.add(&appearance);

    // Sidebar
    let sidebar = adw::PreferencesGroup::new();
    sidebar.set_title(tr("Sidebar"));
    let folder = switch(
        "Show folder path under session name",
        None,
        settings.show_folder_path,
    );
    on_switch(app, &folder, |app, on| {
        app.update_settings(|s| s.show_folder_path = on);
        app.refresh_sidebar();
    });
    sidebar.add(&folder);
    let labels: Vec<&str> = USAGE_INTERVALS.iter().map(|(_, l)| *l).collect();
    let index = USAGE_INTERVALS
        .iter()
        .position(|(s, _)| *s == settings.usage_interval)
        .unwrap_or(1);
    let usage = combo("Check Claude limits every", &labels, index);
    let weak = Rc::downgrade(app);
    usage.connect_selected_notify(move |row| {
        let Some(app) = weak.upgrade() else { return };
        let seconds = USAGE_INTERVALS[row.selected() as usize].0;
        app.update_settings(|s| s.usage_interval = seconds);
        crate::usage::rearm(&app);
    });
    sidebar.add(&usage);
    page.add(&sidebar);

    // Files panel
    let files = adw::PreferencesGroup::new();
    files.set_title(tr("Files Panel"));
    let wrap = switch(
        "Wrap long lines in file preview",
        None,
        settings.preview_wrap_lines,
    );
    on_switch(app, &wrap, |app, on| {
        app.update_settings(|s| s.preview_wrap_lines = on);
        crate::ui::panel::selection_changed(app);
    });
    files.add(&wrap);
    page.add(&files);

    // Sessions
    let sessions = adw::PreferencesGroup::new();
    sessions.set_title(tr("Sessions"));
    let resume = switch(
        "Auto-resume agent sessions at launch",
        Some("Off: restored sessions stay stopped until you press ▶. Clicking one only opens its shell."),
        settings.auto_resume,
    );
    on_switch(app, &resume, |app, on| {
        app.update_settings(|s| s.auto_resume = on)
    });
    sessions.add(&resume);
    let on_select = switch(
        "Continue a session when you select it",
        Some("A session restored at launch starts its agent and continues the conversation when you click it. Off: it opens a plain shell and ▶ starts the agent. For a plain terminal any time, right-click a session › Open Terminal Here."),
        settings.resume_on_select,
    );
    on_switch(app, &on_select, |app, on| {
        app.update_settings(|s| s.resume_on_select = on);
        // Sessions not started yet follow the new choice.
        let ids = app.all_tab_ids();
        for id in ids {
            if app.terminal(id).is_none() {
                app.update_rt(id, |rt| {
                    if rt.exited {
                        rt.resume_on_select = on;
                    }
                });
            }
        }
    });
    sessions.add(&on_select);
    const IDLE: [(u32, &str); 6] = [
        (0, "Never"),
        (5, "5 minutes"),
        (10, "10 minutes"),
        (15, "15 minutes"),
        (30, "30 minutes"),
        (60, "1 hour"),
    ];
    let labels: Vec<&str> = IDLE.iter().map(|(_, l)| *l).collect();
    let index = IDLE
        .iter()
        .position(|(m, _)| *m == settings.idle_close_minutes)
        .unwrap_or(0);
    let idle = combo("Close idle agent sessions after", &labels, index);
    idle.set_subtitle("Frees the memory of Claude Code and Codex sessions nobody uses. The last minutes count down in the sidebar. Select the session to continue its conversation. Sessions that are working, waiting for you, selected or plain shells stay open.");
    let weak = Rc::downgrade(app);
    idle.connect_selected_notify(move |row| {
        let Some(app) = weak.upgrade() else { return };
        let minutes = IDLE[row.selected() as usize].0;
        app.update_settings(|s| s.idle_close_minutes = minutes);
    });
    sessions.add(&idle);
    let quit = switch(
        "Confirm before quitting while sessions are running",
        Some("Quitting stops every session. Saved Claude Code and Codex conversations can be continued on the next launch."),
        settings.confirm_quit,
    );
    on_switch(app, &quit, |app, on| {
        app.update_settings(|s| s.confirm_quit = on)
    });
    sessions.add(&quit);
    let skip = switch(
        "Skip permission prompts",
        Some("Claude Code starts with --dangerously-skip-permissions, and Codex runs without approvals or a sandbox. Agents can then change any file and run any command without asking. Applies to sessions started from now on."),
        settings.skip_permissions,
    );
    on_switch(app, &skip, |app, on| {
        app.update_settings(|s| s.skip_permissions = on)
    });
    sessions.add(&skip);
    page.add(&sessions);

    // Notifications
    let notifications = adw::PreferencesGroup::new();
    notifications.set_title(tr("Notifications"));
    notifications.set_description(Some(
        tr("Shown while you're looking at another session or another app. Claude Code sends its own when its notification channel is ghostty (/config)."),
    ));
    let needs = switch("When a session needs you", None, settings.notify_needs_you);
    on_switch(app, &needs, |app, on| {
        app.update_settings(|s| s.notify_needs_you = on)
    });
    notifications.add(&needs);
    let done = switch("When a session finishes", None, settings.notify_done);
    on_switch(app, &done, |app, on| {
        app.update_settings(|s| s.notify_done = on)
    });
    notifications.add(&done);
    let programs = switch(
        "When a program in a session sends one",
        None,
        settings.notify_programs,
    );
    on_switch(app, &programs, |app, on| {
        app.update_settings(|s| s.notify_programs = on)
    });
    notifications.add(&programs);
    let style = combo(
        "Style",
        &["Banner and sound", "Sound only"],
        usize::from(settings.notify_style == "sound"),
    );
    let weak = Rc::downgrade(app);
    style.connect_selected_notify(move |row| {
        let Some(app) = weak.upgrade() else { return };
        let style = if row.selected() == 1 {
            "sound"
        } else {
            "banner"
        };
        app.update_settings(|s| s.notify_style = style.into());
    });
    notifications.add(&style);
    let labels: Vec<&str> = SOUNDS.iter().map(|(l, _)| *l).collect();
    let index = SOUNDS
        .iter()
        .position(|(l, _)| *l == settings.notify_sound)
        .unwrap_or(0);
    let sound = combo("Sound", &labels, index);
    let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
    play.set_valign(gtk::Align::Center);
    play.add_css_class("flat");
    play.set_tooltip_text(Some(tr("Play")));
    let s = sound.clone();
    play.connect_clicked(move |_| play_sound(SOUNDS[s.selected() as usize].0));
    sound.add_suffix(&play);
    let weak = Rc::downgrade(app);
    sound.connect_selected_notify(move |row| {
        let Some(app) = weak.upgrade() else { return };
        let label = SOUNDS[row.selected() as usize].0.to_string();
        app.update_settings(|s| s.notify_sound = label);
    });
    notifications.add(&sound);
    page.add(&notifications);

    dialog.add(&page);
    dialog.present(Some(&app.ui.window));
}
