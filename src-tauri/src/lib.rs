mod commands;

use commands::{
    arrange_pins, close_all_pins, close_image, create_pin, create_session, deck_step,
    delete_session, focus_pin, focus_pin_edit,
    get_deck_summary, get_pin_view, list_sessions, quit_app, rename_session, replace_image,
    hide_pins, resize_pin, reveal_pins, set_image_click_through, set_image_collapsed,
    set_image_color, set_image_favorite, set_image_note, set_image_opacity,
    set_image_pos, set_image_scale, set_mode, set_session_starred, set_text_editing,
    switch_session, toggle_click_through_all, toggle_control, PinStore,
};
use commands::pins;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(PinStore::default())
        .setup(|app| {
            // Open the SQLite session store, load the active session into the
            // deck WITHOUT showing pins ("launch quiet"), and manage the conn.
            pins::init_store(app.handle());

            // Global shortcuts:
            //   ⌥⌘V → pin the current clipboard image (new pin)
            //   ⌥⌘C → toggle click-through on all pins (escape hatch)
            //   ⌥⌘P → show / hide the control panel
            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};
                let paste = Shortcut::new(Some(Modifiers::ALT | Modifiers::SUPER), Code::KeyV);
                let through = Shortcut::new(Some(Modifiers::ALT | Modifiers::SUPER), Code::KeyC);
                let panel = Shortcut::new(Some(Modifiers::ALT | Modifiers::SUPER), Code::KeyP);
                app.handle().plugin(
                    tauri_plugin_global_shortcut::Builder::new()
                        .with_shortcuts([paste, through, panel])?
                        .with_handler(move |app, shortcut, event| {
                            if event.state() != ShortcutState::Pressed {
                                return;
                            }
                            if shortcut == &paste {
                                if let Err(e) = pins::create_pin_internal(app) {
                                    notify_error(app, &e);
                                }
                            } else if shortcut == &through {
                                pins::toggle_click_through_all_internal(app);
                            } else if shortcut == &panel {
                                pins::toggle_control_internal(app);
                            }
                        })
                        .build(),
                )?;
            }

            // Convert every floating window (control + pin pool) into a
            // non-activating NSPanel so they ride over fullscreen Spaces, then
            // reveal the control panel (pins stay hidden until you paste).
            #[cfg(target_os = "macos")]
            {
                app.handle().plugin(tauri_nspanel::init())?;
                pins::convert_to_panel(app.handle(), pins::CONTROL_LABEL);
                for label in pins::PIN_LABELS {
                    pins::convert_to_panel(app.handle(), label);
                }
                // Let panels become the main window so WKWebView text inputs
                // (the note field) reliably take focus / show a caret. Must run
                // after the first to_panel() above registered the class.
                pins::patch_panel_focusable();
                // Robust ← / → / ESC for the single-mode viewer: an app-local
                // NSEvent monitor that catches keys whenever a PinShot panel is
                // key, independent of the flaky WKWebView first-responder grab.
                pins::install_key_monitor(app.handle());
            }
            pins::show_control_initial(app.handle());

            // Tray: reliable Show/Hide, New Pin, Quit even when panels are hidden.
            #[cfg(desktop)]
            {
                use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
                use tauri::tray::TrayIconBuilder;

                if let Some(icon) = app.default_window_icon().cloned() {
                    let new_pin =
                        MenuItem::with_id(app, "new_pin", "New Pin (⌥⌘V)", true, None::<&str>)?;
                    let show_hide = MenuItem::with_id(
                        app,
                        "show_hide",
                        "Show / Hide Controls (⌥⌘P)",
                        true,
                        None::<&str>,
                    )?;
                    let close_all =
                        MenuItem::with_id(app, "close_all", "Close All Pins", true, None::<&str>)?;
                    let quit = MenuItem::with_id(app, "quit", "Quit PinShot", true, None::<&str>)?;
                    let sep = PredefinedMenuItem::separator(app)?;
                    let menu =
                        Menu::with_items(app, &[&new_pin, &show_hide, &close_all, &sep, &quit])?;

                    let _tray = TrayIconBuilder::with_id("pinshot-tray")
                        .icon(icon)
                        .tooltip("PinShot")
                        .menu(&menu)
                        .show_menu_on_left_click(true)
                        .on_menu_event(|app, event| match event.id.as_ref() {
                            "new_pin" => {
                                if let Err(e) = pins::create_pin_internal(app) {
                                    notify_error(app, &e);
                                }
                            }
                            "show_hide" => pins::toggle_control_internal(app),
                            "close_all" => {
                                // Confirm — this permanently removes the active
                                // session's images (the tray bypasses the
                                // in-app confirmation dialog).
                                use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
                                let ok = app
                                    .dialog()
                                    .message(
                                        "Close all pins? This permanently removes the current session's images.",
                                    )
                                    .title("PinShot")
                                    .buttons(MessageDialogButtons::OkCancelCustom(
                                        "Close all".into(),
                                        "Cancel".into(),
                                    ))
                                    .blocking_show();
                                if ok {
                                    let store = app.state::<PinStore>();
                                    close_all_pins(app.clone(), store);
                                }
                            }
                            "quit" => app.exit(0),
                            _ => {}
                        })
                        .build(app)?;
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            create_pin,
            replace_image,
            get_pin_view,
            get_deck_summary,
            resize_pin,
            set_image_pos,
            set_image_scale,
            set_image_opacity,
            set_image_collapsed,
            set_image_favorite,
            set_image_note,
            set_image_color,
            set_text_editing,
            close_image,
            close_all_pins,
            set_image_click_through,
            toggle_click_through_all,
            set_mode,
            deck_step,
            focus_pin,
            focus_pin_edit,
            list_sessions,
            create_session,
            switch_session,
            rename_session,
            set_session_starred,
            delete_session,
            reveal_pins,
            hide_pins,
            arrange_pins,
            toggle_control,
            quit_app,
        ])
        .build(tauri::generate_context!())
        .expect("error while building PinShot")
        .run(|_app, _event| {
            // macOS: clicking the Dock icon fires Reopen. When the control panel
            // was hidden, re-show it in place so it returns to the same spot.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                pins::show_control(_app);
            }
        });
}

/// Surface a clipboard / pin error to the user (the panel is non-activating, so
/// a plain `window.alert` would be a no-op — use the dialog plugin).
fn notify_error(app: &tauri::AppHandle, msg: &str) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
    app.dialog()
        .message(msg)
        .title("PinShot")
        .kind(MessageDialogKind::Warning)
        .blocking_show();
}
