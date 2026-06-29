//! Linux input capture and injection.
//!
//! Capture uses evdev: we open the mouse's `/dev/input/event*` node and
//! `EVIOCGRAB` it so the kernel stops delivering its events to the display
//! server (the local cursor freezes). We read relative `REL_X`/`REL_Y` deltas,
//! accumulate them against the local screen bounds to track a virtual cursor
//! position, and run the same edge-crossing logic as the macOS/Windows backends.
//!
//! While the cursor is local (no active remote target), we re-emit every
//! consumed relative event through a uinput virtual mouse so the local pointer
//! still moves. While a remote target is active, we forward deltas over QUIC
//! instead and swallow them locally (the local cursor stays parked at the edge).
//!
//! Injection (receiving side) writes synthesized events to a uinput virtual
//! keyboard/mouse.
//!
//! Both paths are kernel-level and work under Wayland compositors like niri
//! that block display-server-level input capture.

#![cfg(target_os = "linux")]

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use evdev::{
    AttributeSet, Device, EventType, InputEvent, KeyCode, RelativeAxisCode, SynchronizationCode,
    uinput::VirtualDeviceBuilder,
};

use crate::{
    input::{
        self, clear_clipboard_target, current_input_targets, crossing_target,
        local_anchor_point, local_return_point, mark_mouse_move_sent,
        release_remote_buttons, remote_button_is_down, reset_mouse_move_timer,
        reset_remote_button_mask, send_remote_cursor_park, send_remote_mouse_move,
        set_control_clipboard_target, should_send_mouse_move, track_forwarded_key,
        update_active_remote_screen, update_remote_button_mask, ActiveTarget, ClipboardTarget,
        Edge,
    },
    quic_transport, shared_input::{InputEvent as WireInputEvent, MouseButton},
    LayoutState, NativeStageStatus,
};

const SCROLL_WHEEL_UNIT: i32 = 120;

/// Single shared uinput device used to re-emit local mouse movement while not
/// controlling a remote, and to inject received remote events.
static UINPUT_DEVICE: Mutex<Option<UinputDevice>> = Mutex::new(None);

struct UinputDevice {
    mouse: evdev::uinput::VirtualDevice,
    keyboard: evdev::uinput::VirtualDevice,
}

/// Tracks which Windows-VK key codes are currently held down on the injection
/// side, so they can be released when receiving stops.
static INJECTED_KEYS: Mutex<Vec<u16>> = Mutex::new(Vec::new());

pub fn ensure_uinput_devices() -> Result<(), String> {
    let mut guard = UINPUT_DEVICE.lock().map_err(|e| e.to_string())?;
    if guard.is_some() {
        return Ok(());
    }

    let mut mouse_keys = AttributeSet::<KeyCode>::new();
    mouse_keys.insert(KeyCode::BTN_LEFT);
    mouse_keys.insert(KeyCode::BTN_RIGHT);
    mouse_keys.insert(KeyCode::BTN_MIDDLE);

    let mut mouse_rel = AttributeSet::<RelativeAxisCode>::new();
    mouse_rel.insert(RelativeAxisCode::REL_X);
    mouse_rel.insert(RelativeAxisCode::REL_Y);
    mouse_rel.insert(RelativeAxisCode::REL_WHEEL);
    mouse_rel.insert(RelativeAxisCode::REL_HWHEEL);

    let mouse = VirtualDeviceBuilder::new()
        .map_err(|e| format!("uinput mouse open: {e}"))?
        .name("mkvm-mouse")
        .with_keys(&mouse_keys)
        .map_err(|e| format!("uinput mouse keys: {e}"))?
        .with_relative_axes(&mouse_rel)
        .map_err(|e| format!("uinput mouse rel: {e}"))?
        .build()
        .map_err(|e| format!("uinput mouse build: {e}"))?;

    let keyboard = build_keyboard_uinput()?;

    *guard = Some(UinputDevice { mouse, keyboard });
    Ok(())
}

fn build_keyboard_uinput() -> Result<evdev::uinput::VirtualDevice, String> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for code in supported_keyboard_codes() {
        keys.insert(code);
    }

    VirtualDeviceBuilder::new()
        .map_err(|e| format!("uinput keyboard open: {e}"))?
        .name("mkvm-keyboard")
        .with_keys(&keys)
        .map_err(|e| format!("uinput keyboard keys: {e}"))?
        .build()
        .map_err(|e| format!("uinput keyboard build: {e}"))
}

/// All evdev `KeyCode`s we map from/to Windows virtual-key codes.
fn supported_keyboard_codes() -> Vec<KeyCode> {
    [
        KeyCode::KEY_ESC, KeyCode::KEY_1, KeyCode::KEY_2, KeyCode::KEY_3, KeyCode::KEY_4,
        KeyCode::KEY_5, KeyCode::KEY_6, KeyCode::KEY_7, KeyCode::KEY_8, KeyCode::KEY_9,
        KeyCode::KEY_0, KeyCode::KEY_MINUS, KeyCode::KEY_EQUAL, KeyCode::KEY_BACKSPACE,
        KeyCode::KEY_TAB, KeyCode::KEY_Q, KeyCode::KEY_W, KeyCode::KEY_E, KeyCode::KEY_R,
        KeyCode::KEY_T, KeyCode::KEY_Y, KeyCode::KEY_U, KeyCode::KEY_I, KeyCode::KEY_O,
        KeyCode::KEY_P, KeyCode::KEY_LEFTBRACE, KeyCode::KEY_RIGHTBRACE, KeyCode::KEY_ENTER,
        KeyCode::KEY_LEFTCTRL, KeyCode::KEY_A, KeyCode::KEY_S, KeyCode::KEY_D, KeyCode::KEY_F,
        KeyCode::KEY_G, KeyCode::KEY_H, KeyCode::KEY_J, KeyCode::KEY_K, KeyCode::KEY_L,
        KeyCode::KEY_SEMICOLON, KeyCode::KEY_APOSTROPHE, KeyCode::KEY_GRAVE,
        KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_BACKSLASH, KeyCode::KEY_Z, KeyCode::KEY_X,
        KeyCode::KEY_C, KeyCode::KEY_V, KeyCode::KEY_B, KeyCode::KEY_N, KeyCode::KEY_M,
        KeyCode::KEY_COMMA, KeyCode::KEY_DOT, KeyCode::KEY_SLASH, KeyCode::KEY_RIGHTSHIFT,
        KeyCode::KEY_KPASTERISK, KeyCode::KEY_LEFTALT, KeyCode::KEY_SPACE, KeyCode::KEY_CAPSLOCK,
        KeyCode::KEY_F1, KeyCode::KEY_F2, KeyCode::KEY_F3, KeyCode::KEY_F4, KeyCode::KEY_F5,
        KeyCode::KEY_F6, KeyCode::KEY_F7, KeyCode::KEY_F8, KeyCode::KEY_F9, KeyCode::KEY_F10,
        KeyCode::KEY_F11, KeyCode::KEY_F12, KeyCode::KEY_NUMLOCK, KeyCode::KEY_SCROLLLOCK,
        KeyCode::KEY_KP7, KeyCode::KEY_KP8, KeyCode::KEY_KP9, KeyCode::KEY_KPMINUS,
        KeyCode::KEY_KP4, KeyCode::KEY_KP5, KeyCode::KEY_KP6, KeyCode::KEY_KPPLUS,
        KeyCode::KEY_KP1, KeyCode::KEY_KP2, KeyCode::KEY_KP3, KeyCode::KEY_KP0,
        KeyCode::KEY_KPDOT, KeyCode::KEY_RO, KeyCode::KEY_KPENTER, KeyCode::KEY_RIGHTCTRL,
        KeyCode::KEY_KPSLASH, KeyCode::KEY_SYSRQ, KeyCode::KEY_RIGHTALT, KeyCode::KEY_LINEFEED,
        KeyCode::KEY_HOME, KeyCode::KEY_UP, KeyCode::KEY_PAGEUP, KeyCode::KEY_LEFT,
        KeyCode::KEY_RIGHT, KeyCode::KEY_END, KeyCode::KEY_DOWN, KeyCode::KEY_PAGEDOWN,
        KeyCode::KEY_INSERT, KeyCode::KEY_DELETE, KeyCode::KEY_PAUSE, KeyCode::KEY_KPEQUAL,
        KeyCode::KEY_KPPLUSMINUS, KeyCode::KEY_FIND, KeyCode::KEY_UNDO, KeyCode::KEY_FRONT,
        KeyCode::KEY_COPY, KeyCode::KEY_OPEN, KeyCode::KEY_PASTE, KeyCode::KEY_CUT,
        KeyCode::KEY_HELP, KeyCode::KEY_MENU, KeyCode::KEY_CALC, KeyCode::KEY_SETUP,
        KeyCode::KEY_SLEEP, KeyCode::KEY_WAKEUP, KeyCode::KEY_LEFTMETA, KeyCode::KEY_RIGHTMETA,
        KeyCode::KEY_MAIL, KeyCode::KEY_BOOKMARKS, KeyCode::KEY_COMPUTER, KeyCode::KEY_BACK,
        KeyCode::KEY_FORWARD, KeyCode::KEY_EJECTCD, KeyCode::KEY_NEXTSONG,
        KeyCode::KEY_PLAYPAUSE, KeyCode::KEY_PREVIOUSSONG, KeyCode::KEY_STOPCD,
        KeyCode::KEY_RECORD, KeyCode::KEY_REWIND, KeyCode::KEY_PHONE, KeyCode::KEY_CONFIG,
        KeyCode::KEY_HOMEPAGE, KeyCode::KEY_REFRESH, KeyCode::KEY_EXIT, KeyCode::KEY_EDIT,
        KeyCode::KEY_NEW, KeyCode::KEY_SAVE, KeyCode::KEY_DOCUMENTS,
        KeyCode::KEY_BRIGHTNESSDOWN, KeyCode::KEY_BRIGHTNESSUP, KeyCode::KEY_MEDIA,
        KeyCode::KEY_DASHBOARD, KeyCode::KEY_SCALE, KeyCode::KEY_CYCLEWINDOWS,
        KeyCode::KEY_FN, KeyCode::KEY_FN_ESC, KeyCode::KEY_ZENKAKUHANKAKU,
        KeyCode::KEY_KATAKANA, KeyCode::KEY_HIRAGANA, KeyCode::KEY_HENKAN,
        KeyCode::KEY_KATAKANAHIRAGANA, KeyCode::KEY_MUHENKAN, KeyCode::KEY_KPJPCOMMA,
        KeyCode::KEY_MUTE, KeyCode::KEY_VOLUMEDOWN, KeyCode::KEY_VOLUMEUP, KeyCode::KEY_POWER,
    ]
    .into_iter()
    .collect::<HashSet<_>>()
    .into_iter()
    .collect()
}

struct LinuxCaptureContext {
    quic_transport: quic_transport::TransportHandle,
    layout_state: Arc<Mutex<LayoutState>>,
    native_layout: LayoutState,
    active: Mutex<Option<ActiveTarget>>,
    remote_active: Arc<AtomicBool>,
    clipboard_target: Arc<Mutex<Option<ClipboardTarget>>>,
    input_events: Arc<AtomicU64>,
    last_mouse_move_sent: Mutex<Option<Instant>>,
    remote_button_mask: AtomicU64,
    pressed_keys: Mutex<Vec<u16>>,
    just_crossed: AtomicBool,
    /// Virtual cursor in native layout coordinates, accumulated from rel deltas.
    cursor: Mutex<VirtualCursor>,
}

#[derive(Clone, Copy)]
struct VirtualCursor {
    x: f64,
    y: f64,
}

pub fn start_platform_capture(
    targets: Vec<crate::input::InputTarget>,
    layout_state: Arc<Mutex<LayoutState>>,
    native_layout: LayoutState,
    quic_transport: quic_transport::TransportHandle,
    stop: Arc<AtomicBool>,
    remote_active: Arc<AtomicBool>,
    _main_window_visible: Arc<AtomicBool>,
    _main_window_focused: Arc<AtomicBool>,
    clipboard_target: Arc<Mutex<Option<ClipboardTarget>>>,
    input_events: Arc<AtomicU64>,
) -> NativeStageStatus {
    if let Err(error) = ensure_uinput_devices() {
        return NativeStageStatus {
            state: "error".into(),
            detail: format!(
                "无法初始化 uinput 设备: {error}。确保 /dev/uinput 存在且当前用户有读写权限（通常需要加入 input 组，或配置 udev 规则）。"
            ),
        };
    }

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let target_count = targets.len();

    // Seed the virtual cursor at the center of the primary screen.
    let initial_cursor = native_layout
        .devices
        .iter()
        .find(|d| d.role == "local")
        .and_then(|d| d.screens.iter().find(|s| s.is_primary).or(d.screens.first()))
        .map(|s| VirtualCursor {
            x: s.x as f64 + s.width as f64 / 2.0,
            y: s.y as f64 + s.height as f64 / 2.0,
        })
        .unwrap_or(VirtualCursor { x: 0.0, y: 0.0 });

    thread::spawn(move || {
        let context = Arc::new(LinuxCaptureContext {
            quic_transport,
            layout_state,
            native_layout,
            active: Mutex::new(None),
            remote_active,
            clipboard_target,
            input_events,
            last_mouse_move_sent: Mutex::new(None),
            remote_button_mask: AtomicU64::new(0),
            pressed_keys: Mutex::new(Vec::new()),
            just_crossed: AtomicBool::new(false),
            cursor: Mutex::new(initial_cursor),
        });

        // Discover and grab mouse devices that emit REL_X/REL_Y.
        let mice = match discover_mouse_devices() {
            Ok(mice) if !mice.is_empty() => mice,
            Ok(_) => {
                let _ = ready_tx.send(Err(
                    "没有在 /dev/input 找到可用的鼠标设备。请检查 input 组权限。".into(),
                ));
                return;
            }
            Err(error) => {
                let _ = ready_tx.send(Err(format!("枚举 evdev 鼠标设备失败: {error}")));
                return;
            }
        };

        let mut grabbed: Vec<Device> = Vec::new();
        for mut dev in mice {
            if dev.grab().is_ok() {
                grabbed.push(dev);
            }
        }

        if grabbed.is_empty() {
            let _ = ready_tx.send(Err(
                "无法独占（grab）任何鼠标设备。可能缺少 /dev/input 读权限，或其它程序正在占用。".into(),
            ));
            return;
        }

        let _ = ready_tx.send(Ok(()));

        let mut pending_dx: f64 = 0.0;
        let mut pending_dy: f64 = 0.0;
        let mut button_change: Option<(MouseButton, bool)> = None;
        let mut scroll_delta: (i32, i32) = (0, 0);

        while !stop.load(Ordering::Relaxed) {
            for dev in &mut grabbed {
                match dev.fetch_events() {
                    Ok(events) => {
                        for ev in events {
                            handle_evdev_event(
                                &context,
                                ev,
                                &mut pending_dx,
                                &mut pending_dy,
                                &mut button_change,
                                &mut scroll_delta,
                            );
                        }
                    }
                    Err(error) => {
                        if error.kind() != std::io::ErrorKind::WouldBlock {
                            log::debug!("evdev read error: {error}");
                        }
                    }
                }
            }
            // Flush accumulated deltas per frame.
            if pending_dx != 0.0 || pending_dy != 0.0 {
                handle_mouse_move(&context, pending_dx, pending_dy);
                pending_dx = 0.0;
                pending_dy = 0.0;
            }
            if let Some((button, down)) = button_change.take() {
                handle_mouse_button(&context, button, down);
            }
            if scroll_delta.0 != 0 || scroll_delta.1 != 0 {
                handle_scroll(&context, scroll_delta.0, scroll_delta.1);
                scroll_delta = (0, 0);
            }
            thread::sleep(Duration::from_millis(2));
        }

        // Release grabbed devices and remote control on exit.
        release_linux_remote_control(&context);
        for mut dev in grabbed {
            let _ = dev.ungrab();
        }
    });

    match ready_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => NativeStageStatus {
            state: "ready".into(),
            detail: format!("控制端已就绪，{target_count} 条远端贴边可用于鼠标和键盘切换。"),
        },
        Ok(Err(error)) => NativeStageStatus {
            state: "error".into(),
            detail: error,
        },
        Err(_) => NativeStageStatus {
            state: "error".into(),
            detail: "Linux input capture did not become ready.".into(),
        },
    }
}

fn discover_mouse_devices() -> std::io::Result<Vec<Device>> {
    let mut mice = Vec::new();
    for path in evdev::enumerate().map(|(path, _)| path) {
        let Ok(dev) = Device::open(&path) else {
            continue;
        };
        let is_mouse = dev
            .supported_relative_axes()
            .map(|axes| {
                axes.contains(RelativeAxisCode::REL_X) && axes.contains(RelativeAxisCode::REL_Y)
            })
            .unwrap_or(false)
            && dev
                .supported_keys()
                .map(|keys| {
                    keys.contains(KeyCode::BTN_LEFT) || keys.contains(KeyCode::BTN_RIGHT)
                })
                .unwrap_or(false);
        if is_mouse {
            mice.push(dev);
        }
    }
    Ok(mice)
}

fn handle_evdev_event(
    context: &LinuxCaptureContext,
    ev: InputEvent,
    pending_dx: &mut f64,
    pending_dy: &mut f64,
    button_change: &mut Option<(MouseButton, bool)>,
    scroll_delta: &mut (i32, i32),
) {
    match ev.destructure() {
        evdev::EventSummary::RelativeAxis(_, RelativeAxisCode::REL_X, value) => {
            *pending_dx += value as f64;
        }
        evdev::EventSummary::RelativeAxis(_, RelativeAxisCode::REL_Y, value) => {
            *pending_dy += value as f64;
        }
        evdev::EventSummary::RelativeAxis(_, RelativeAxisCode::REL_WHEEL, value) => {
            // evdev wheel units are clicks; mkvm wire uses 120-per-notch multiples.
            scroll_delta.1 += value * SCROLL_WHEEL_UNIT;
        }
        evdev::EventSummary::RelativeAxis(_, RelativeAxisCode::REL_HWHEEL, value) => {
            scroll_delta.0 += value * SCROLL_WHEEL_UNIT;
        }
        evdev::EventSummary::Key(_, KeyCode::BTN_LEFT, value) => {
            *button_change = Some((MouseButton::Left, value == 1));
        }
        evdev::EventSummary::Key(_, KeyCode::BTN_RIGHT, value) => {
            *button_change = Some((MouseButton::Right, value == 1));
        }
        evdev::EventSummary::Key(_, KeyCode::BTN_MIDDLE, value) => {
            *button_change = Some((MouseButton::Middle, value == 1));
        }
        evdev::EventSummary::Key(_, code, value) => {
            if let Some(key_code) = evdev_key_to_windows_vk(code) {
                handle_key(context, key_code, value == 1);
            } else {
                // Unknown key: if not controlling remote, re-emit to uinput so it
                // still reaches the local session.
                let active = context
                    .active
                    .lock()
                    .map(|a| a.is_some())
                    .unwrap_or(false);
                if !active {
                    reemit_key(code, value == 1);
                }
            }
        }
        _ => {}
    }
}

fn handle_mouse_move(context: &LinuxCaptureContext, dx: f64, dy: f64) {
    if dx == 0.0 && dy == 0.0 {
        return;
    }

    let mut active = match context.active.lock() {
        Ok(active) => active,
        Err(_) => return,
    };

    if let Some(active_target) = active.as_mut() {
        // Currently controlling remote: forward deltas, swallow locally.
        if context.just_crossed.swap(false, Ordering::Relaxed)
            && should_ignore_initial_anchor_warp_delta(active_target.target.edge, dx, dy)
        {
            return;
        }
        active_target.x += dx;
        active_target.y += dy;

        if update_active_remote_screen(active_target, dx, dy, &context.layout_state) {
            // Returning to local.
            let point = local_return_point(active_target);
            let target = active_target.target.clone();
            let _ = send_remote_cursor_park(
                &context.quic_transport,
                active_target,
                &context.layout_state,
                &context.input_events,
            );
            *active = None;
            context.remote_active.store(false, Ordering::Relaxed);
            release_forwarded_keys_linux(context, &target);
            release_remote_buttons(
                &context.quic_transport,
                &target,
                &context.remote_button_mask,
                &context.layout_state,
                &context.input_events,
            );
            reset_mouse_move_timer(&context.last_mouse_move_sent);
            // Park local virtual cursor at the return point.
            if let Ok(mut cursor) = context.cursor.lock() {
                cursor.x = point.0;
                cursor.y = point.1;
            }
            return;
        }

        active_target.x = active_target
            .x
            .clamp(0.0, (active_target.current_screen.width - 1) as f64);
        active_target.y = active_target
            .y
            .clamp(0.0, (active_target.current_screen.height - 1) as f64);
        let dragging = remote_button_is_down(&context.remote_button_mask);
        if should_send_mouse_move(&context.last_mouse_move_sent, dragging) {
            if !send_remote_mouse_move(
                &context.quic_transport,
                active_target,
                &context.layout_state,
                &context.input_events,
            ) {
                *active = None;
                context.remote_active.store(false, Ordering::Relaxed);
                clear_clipboard_target(&context.clipboard_target);
                reset_mouse_move_timer(&context.last_mouse_move_sent);
                reset_remote_button_mask(&context.remote_button_mask);
                if let Ok(mut keys) = context.pressed_keys.lock() {
                    keys.clear();
                }
                return;
            }
        }
        return;
    }

    // Not controlling: update virtual cursor and check for edge crossing.
    let (cursor_x, cursor_y) = {
        let Ok(mut cursor) = context.cursor.lock() else {
            return;
        };
        cursor.x += dx;
        cursor.y += dy;
        let (nx, ny) = clamp_cursor(&context.native_layout, cursor.x, cursor.y);
        cursor.x = nx;
        cursor.y = ny;
        (nx, ny)
    };

    let targets = current_input_targets(&context.layout_state, &context.native_layout);
    if let Some(active_target) =
        crossing_target(&targets, cursor_x, cursor_y, dx, dy, &context.layout_state)
    {
        let anchor = local_anchor_point(&active_target);
        if !send_remote_mouse_move(
            &context.quic_transport,
            &active_target,
            &context.layout_state,
            &context.input_events,
        ) {
            reset_mouse_move_timer(&context.last_mouse_move_sent);
            reset_remote_button_mask(&context.remote_button_mask);
            return;
        }
        mark_mouse_move_sent(&context.last_mouse_move_sent);
        reset_remote_button_mask(&context.remote_button_mask);
        context.remote_active.store(true, Ordering::Relaxed);
        set_control_clipboard_target(&context.clipboard_target, &active_target, &context.layout_state);
        if let Ok(mut cursor) = context.cursor.lock() {
            cursor.x = anchor.0;
            cursor.y = anchor.1;
        }
        *active = Some(active_target);
        context.just_crossed.store(true, Ordering::Relaxed);
        return;
    }

    // No crossing: re-emit the relative movement to the local session so the
    // local pointer moves normally.
    reemit_relative(dx as i32, dy as i32);
}

fn handle_mouse_button(context: &LinuxCaptureContext, button: MouseButton, down: bool) {
    let active = match context.active.lock() {
        Ok(active) => active,
        Err(_) => return,
    };
    let Some(active_target) = active.as_ref().cloned() else {
        // Not controlling: re-emit the button to local uinput.
        reemit_button(button, down);
        return;
    };
    drop(active);

    if !send_remote_mouse_move(
        &context.quic_transport,
        &active_target,
        &context.layout_state,
        &context.input_events,
    ) {
        return;
    }
    mark_mouse_move_sent(&context.last_mouse_move_sent);

    let sent = input::send_packet_public(
        &context.quic_transport,
        &active_target.target,
        WireInputEvent::MouseButton { button, down },
        &context.layout_state,
        &context.input_events,
    );
    if sent {
        update_remote_button_mask(&context.remote_button_mask, button, down);
    }
}

fn handle_scroll(context: &LinuxCaptureContext, delta_x: i32, delta_y: i32) {
    let active = match context.active.lock() {
        Ok(active) => active,
        Err(_) => return,
    };
    let Some(active_target) = active.as_ref().cloned() else {
        reemit_scroll(delta_x, delta_y);
        return;
    };
    drop(active);

    if !send_remote_mouse_move(
        &context.quic_transport,
        &active_target,
        &context.layout_state,
        &context.input_events,
    ) {
        return;
    }
    mark_mouse_move_sent(&context.last_mouse_move_sent);

    let _ = input::send_packet_public(
        &context.quic_transport,
        &active_target.target,
        WireInputEvent::Scroll { delta_x, delta_y },
        &context.layout_state,
        &context.input_events,
    );
}

fn handle_key(context: &LinuxCaptureContext, key_code: u16, down: bool) {
    let active = context
        .active
        .lock()
        .map(|active| active.as_ref().map(|a| a.target.clone()))
        .unwrap_or(None);

    let Some(target) = active else {
        // Not controlling: re-emit to local uinput.
        reemit_key_code(key_code, down);
        return;
    };

    let sent = input::send_packet_public(
        &context.quic_transport,
        &target,
        WireInputEvent::Key { key_code, down },
        &context.layout_state,
        &context.input_events,
    );
    if sent {
        track_forwarded_key(&context.pressed_keys, key_code, down);
    }
}

fn release_forwarded_keys_linux(
    context: &LinuxCaptureContext,
    target: &crate::input::InputTarget,
) {
    let held = context
        .pressed_keys
        .lock()
        .map(|keys| keys.clone())
        .unwrap_or_default();
    for key_code in held {
        input::send_packet_public(
            &context.quic_transport,
            target,
            WireInputEvent::Key {
                key_code,
                down: false,
            },
            &context.layout_state,
            &context.input_events,
        );
    }
    if let Ok(mut keys) = context.pressed_keys.lock() {
        keys.clear();
    }
}

fn release_linux_remote_control(context: &LinuxCaptureContext) {
    let target = context
        .active
        .lock()
        .ok()
        .and_then(|mut active| active.take().map(|a| a.target));

    if let Some(target) = target {
        release_forwarded_keys_linux(context, &target);
        release_remote_buttons(
            &context.quic_transport,
            &target,
            &context.remote_button_mask,
            &context.layout_state,
            &context.input_events,
        );
    } else {
        reset_remote_button_mask(&context.remote_button_mask);
        if let Ok(mut keys) = context.pressed_keys.lock() {
            keys.clear();
        }
    }
    context.remote_active.store(false, Ordering::Relaxed);
    context.just_crossed.store(false, Ordering::Relaxed);
    reset_mouse_move_timer(&context.last_mouse_move_sent);
    clear_clipboard_target(&context.clipboard_target);
}

fn clamp_cursor(native_layout: &LayoutState, x: f64, y: f64) -> (f64, f64) {
    let local_device = native_layout
        .devices
        .iter()
        .find(|d| d.role == "local")
        .or(native_layout.devices.first());

    if let Some(device) = local_device {
        for screen in &device.screens {
            let left = screen.x as f64;
            let right = (screen.x + screen.width) as f64;
            let top = screen.y as f64;
            let bottom = (screen.y + screen.height) as f64;
            if x >= left && x < right && y >= top && y < bottom {
                return (x, y);
            }
        }
        // Clamp to the union of all local screens.
        if let Some(first) = device.screens.first() {
            let min_x = device.screens.iter().map(|s| s.x).min().unwrap_or(first.x);
            let max_x = device
                .screens
                .iter()
                .map(|s| s.x + s.width)
                .max()
                .unwrap_or(first.x + first.width);
            let min_y = device.screens.iter().map(|s| s.y).min().unwrap_or(first.y);
            let max_y = device
                .screens
                .iter()
                .map(|s| s.y + s.height)
                .max()
                .unwrap_or(first.y + first.height);
            return (
                x.clamp(min_x as f64, (max_x - 1) as f64),
                y.clamp(min_y as f64, (max_y - 1) as f64),
            );
        }
    }
    (x, y)
}

fn should_ignore_initial_anchor_warp_delta(edge: Edge, dx: f64, dy: f64) -> bool {
    use crate::input::{CROSSING_AXIS_DOMINANCE, MIN_CROSSING_DELTA};
    match edge {
        Edge::Right => dx <= -MIN_CROSSING_DELTA && dx.abs() >= dy.abs() * CROSSING_AXIS_DOMINANCE,
        Edge::Left => dx >= MIN_CROSSING_DELTA && dx.abs() >= dy.abs() * CROSSING_AXIS_DOMINANCE,
        Edge::Bottom => dy <= -MIN_CROSSING_DELTA && dy.abs() >= dx.abs() * CROSSING_AXIS_DOMINANCE,
        Edge::Top => dy >= MIN_CROSSING_DELTA && dy.abs() >= dx.abs() * CROSSING_AXIS_DOMINANCE,
    }
}

// ---------- uinput re-emission (local passthrough) ----------

fn reemit_relative(dx: i32, dy: i32) {
    let Ok(mut guard) = UINPUT_DEVICE.lock() else {
        return;
    };
    let Some(device) = guard.as_mut() else {
        return;
    };
    let mut events = Vec::new();
    if dx != 0 {
        events.push(InputEvent::new(
            EventType::RELATIVE.0,
            RelativeAxisCode::REL_X.0,
            dx,
        ));
    }
    if dy != 0 {
        events.push(InputEvent::new(
            EventType::RELATIVE.0,
            RelativeAxisCode::REL_Y.0,
            dy,
        ));
    }
    if !events.is_empty() {
        events.push(InputEvent::new(
            EventType::SYNCHRONIZATION.0,
            SynchronizationCode::SYN_REPORT.0,
            0,
        ));
        let _ = device.mouse.emit(&events);
    }
}

fn reemit_button(button: MouseButton, down: bool) {
    let Ok(mut guard) = UINPUT_DEVICE.lock() else {
        return;
    };
    let Some(device) = guard.as_mut() else {
        return;
    };
    let code = match button {
        MouseButton::Left => KeyCode::BTN_LEFT,
        MouseButton::Right => KeyCode::BTN_RIGHT,
        MouseButton::Middle => KeyCode::BTN_MIDDLE,
    };
    let value = if down { 1 } else { 0 };
    let _ = device.mouse.emit(&[
        InputEvent::new(EventType::KEY.0, code.0, value),
        InputEvent::new(EventType::SYNCHRONIZATION.0, SynchronizationCode::SYN_REPORT.0, 0),
    ]);
}

fn reemit_scroll(delta_x: i32, delta_y: i32) {
    let Ok(mut guard) = UINPUT_DEVICE.lock() else {
        return;
    };
    let Some(device) = guard.as_mut() else {
        return;
    };
    let mut events = Vec::new();
    if delta_y != 0 {
        events.push(InputEvent::new(
            EventType::RELATIVE.0,
            RelativeAxisCode::REL_WHEEL.0,
            delta_y / SCROLL_WHEEL_UNIT,
        ));
    }
    if delta_x != 0 {
        events.push(InputEvent::new(
            EventType::RELATIVE.0,
            RelativeAxisCode::REL_HWHEEL.0,
            delta_x / SCROLL_WHEEL_UNIT,
        ));
    }
    if !events.is_empty() {
        events.push(InputEvent::new(
            EventType::SYNCHRONIZATION.0,
            SynchronizationCode::SYN_REPORT.0,
            0,
        ));
        let _ = device.mouse.emit(&events);
    }
}

fn reemit_key(code: KeyCode, down: bool) {
    let Ok(mut guard) = UINPUT_DEVICE.lock() else {
        return;
    };
    let Some(device) = guard.as_mut() else {
        return;
    };
    let value = if down { 1 } else { 0 };
    let _ = device.keyboard.emit(&[
        InputEvent::new(EventType::KEY.0, code.0, value),
        InputEvent::new(EventType::SYNCHRONIZATION.0, SynchronizationCode::SYN_REPORT.0, 0),
    ]);
}

fn reemit_key_code(key_code: u16, down: bool) {
    if let Some(code) = windows_vk_to_evdev_key(key_code) {
        reemit_key(code, down);
    }
}

// ---------- injection (receiving side) ----------

pub fn inject_mouse_move(x: i32, y: i32, _drag_button: Option<MouseButton>) {
    // uinput mice are relative; move toward absolute (x, y) by emitting a delta
    // from the last injected position.
    static LAST_X: AtomicI32 = AtomicI32::new(0);
    static LAST_Y: AtomicI32 = AtomicI32::new(0);
    let dx = x - LAST_X.swap(x, Ordering::Relaxed);
    let dy = y - LAST_Y.swap(y, Ordering::Relaxed);
    if dx != 0 || dy != 0 {
        reemit_relative(dx, dy);
    }
}

pub fn inject_mouse_button(button: MouseButton, down: bool, _x: i32, _y: i32) {
    reemit_button(button, down);
}

pub fn inject_scroll(delta_x: i32, delta_y: i32) {
    reemit_scroll(delta_x, delta_y);
}

pub fn inject_key(key_code: u16, down: bool) {
    if let Some(code) = windows_vk_to_evdev_key(key_code) {
        reemit_key(code, down);
        if let Ok(mut keys) = INJECTED_KEYS.lock() {
            if down {
                if !keys.contains(&key_code) {
                    keys.push(key_code);
                }
            } else {
                keys.retain(|k| *k != key_code);
            }
        }
    } else {
        log::debug!("inject_key: no evdev keycode for windows vk {key_code:#04x}; dropping");
    }
}

/// Release all keys we injected as held down. Called when receiving stops.
pub fn release_injected_keys() {
    let keys = INJECTED_KEYS.lock().map(|k| k.clone()).unwrap_or_default();
    for key_code in keys {
        if let Some(code) = windows_vk_to_evdev_key(key_code) {
            reemit_key(code, false);
        }
    }
    if let Ok(mut keys) = INJECTED_KEYS.lock() {
        keys.clear();
    }
}

// ---------- keycode mapping ----------

fn evdev_key_to_windows_vk(code: KeyCode) -> Option<u16> {
    Some(match code {
        KeyCode::KEY_ESC => 0x1B,
        KeyCode::KEY_1 => 0x31,
        KeyCode::KEY_2 => 0x32,
        KeyCode::KEY_3 => 0x33,
        KeyCode::KEY_4 => 0x34,
        KeyCode::KEY_5 => 0x35,
        KeyCode::KEY_6 => 0x36,
        KeyCode::KEY_7 => 0x37,
        KeyCode::KEY_8 => 0x38,
        KeyCode::KEY_9 => 0x39,
        KeyCode::KEY_0 => 0x30,
        KeyCode::KEY_MINUS => 0xBD,
        KeyCode::KEY_EQUAL => 0xBB,
        KeyCode::KEY_BACKSPACE => 0x08,
        KeyCode::KEY_TAB => 0x09,
        KeyCode::KEY_Q => 0x51,
        KeyCode::KEY_W => 0x57,
        KeyCode::KEY_E => 0x45,
        KeyCode::KEY_R => 0x52,
        KeyCode::KEY_T => 0x54,
        KeyCode::KEY_Y => 0x59,
        KeyCode::KEY_U => 0x55,
        KeyCode::KEY_I => 0x49,
        KeyCode::KEY_O => 0x4F,
        KeyCode::KEY_P => 0x50,
        KeyCode::KEY_LEFTBRACE => 0xDB,
        KeyCode::KEY_RIGHTBRACE => 0xDD,
        KeyCode::KEY_ENTER => 0x0D,
        KeyCode::KEY_LEFTCTRL => 0xA2,
        KeyCode::KEY_A => 0x41,
        KeyCode::KEY_S => 0x53,
        KeyCode::KEY_D => 0x44,
        KeyCode::KEY_F => 0x46,
        KeyCode::KEY_G => 0x47,
        KeyCode::KEY_H => 0x48,
        KeyCode::KEY_J => 0x4A,
        KeyCode::KEY_K => 0x4B,
        KeyCode::KEY_L => 0x4C,
        KeyCode::KEY_SEMICOLON => 0xBA,
        KeyCode::KEY_APOSTROPHE => 0xDE,
        KeyCode::KEY_GRAVE => 0xC0,
        KeyCode::KEY_LEFTSHIFT => 0xA0,
        KeyCode::KEY_BACKSLASH => 0xDC,
        KeyCode::KEY_Z => 0x5A,
        KeyCode::KEY_X => 0x58,
        KeyCode::KEY_C => 0x43,
        KeyCode::KEY_V => 0x56,
        KeyCode::KEY_B => 0x42,
        KeyCode::KEY_N => 0x4E,
        KeyCode::KEY_M => 0x4D,
        KeyCode::KEY_COMMA => 0xBC,
        KeyCode::KEY_DOT => 0xBE,
        KeyCode::KEY_SLASH => 0xBF,
        KeyCode::KEY_RIGHTSHIFT => 0xA1,
        KeyCode::KEY_KPASTERISK => 0x6A,
        KeyCode::KEY_LEFTALT => 0xA4,
        KeyCode::KEY_SPACE => 0x20,
        KeyCode::KEY_CAPSLOCK => 0x14,
        KeyCode::KEY_F1 => 0x70,
        KeyCode::KEY_F2 => 0x71,
        KeyCode::KEY_F3 => 0x72,
        KeyCode::KEY_F4 => 0x73,
        KeyCode::KEY_F5 => 0x74,
        KeyCode::KEY_F6 => 0x75,
        KeyCode::KEY_F7 => 0x76,
        KeyCode::KEY_F8 => 0x77,
        KeyCode::KEY_F9 => 0x78,
        KeyCode::KEY_F10 => 0x79,
        KeyCode::KEY_F11 => 0x7A,
        KeyCode::KEY_F12 => 0x7B,
        KeyCode::KEY_NUMLOCK => 0x90,
        KeyCode::KEY_SCROLLLOCK => 0x91,
        KeyCode::KEY_KP7 => 0x67,
        KeyCode::KEY_KP8 => 0x68,
        KeyCode::KEY_KP9 => 0x69,
        KeyCode::KEY_KPMINUS => 0x6D,
        KeyCode::KEY_KP4 => 0x64,
        KeyCode::KEY_KP5 => 0x65,
        KeyCode::KEY_KP6 => 0x66,
        KeyCode::KEY_KPPLUS => 0x6B,
        KeyCode::KEY_KP1 => 0x61,
        KeyCode::KEY_KP2 => 0x62,
        KeyCode::KEY_KP3 => 0x63,
        KeyCode::KEY_KP0 => 0x60,
        KeyCode::KEY_KPDOT => 0x6E,
        KeyCode::KEY_KPENTER => 0x6C,
        KeyCode::KEY_RIGHTCTRL => 0xA3,
        KeyCode::KEY_KPSLASH => 0x6F,
        KeyCode::KEY_SYSRQ => 0x2C,
        KeyCode::KEY_RIGHTALT => 0xA5,
        KeyCode::KEY_HOME => 0x24,
        KeyCode::KEY_UP => 0x26,
        KeyCode::KEY_PAGEUP => 0x21,
        KeyCode::KEY_LEFT => 0x25,
        KeyCode::KEY_RIGHT => 0x27,
        KeyCode::KEY_END => 0x23,
        KeyCode::KEY_DOWN => 0x28,
        KeyCode::KEY_PAGEDOWN => 0x22,
        KeyCode::KEY_INSERT => 0x2D,
        KeyCode::KEY_DELETE => 0x2E,
        KeyCode::KEY_PAUSE => 0x13,
        KeyCode::KEY_LEFTMETA => 0x5B,
        KeyCode::KEY_RIGHTMETA => 0x5C,
        KeyCode::KEY_MUTE => 0xAD,
        KeyCode::KEY_VOLUMEDOWN => 0xAE,
        KeyCode::KEY_VOLUMEUP => 0xAF,
        KeyCode::KEY_POWER => 0x5E,
        KeyCode::KEY_SLEEP => 0x5F,
        KeyCode::KEY_WAKEUP => 0xE3,
        KeyCode::KEY_MAIL => 0xB4,
        KeyCode::KEY_BOOKMARKS => 0xAB,
        KeyCode::KEY_COMPUTER => 0xB5,
        KeyCode::KEY_BACK => 0xA6,
        KeyCode::KEY_FORWARD => 0xA7,
        KeyCode::KEY_EJECTCD => 0xE2,
        KeyCode::KEY_NEXTSONG => 0xB0,
        KeyCode::KEY_PLAYPAUSE => 0xB3,
        KeyCode::KEY_PREVIOUSSONG => 0xB1,
        KeyCode::KEY_STOPCD => 0xB2,
        KeyCode::KEY_RECORD => 0xA2,
        KeyCode::KEY_REWIND => 0xA4,
        KeyCode::KEY_HOMEPAGE => 0xAC,
        KeyCode::KEY_REFRESH => 0xA8,
        _ => return None,
    })
}

fn windows_vk_to_evdev_key(vk: u16) -> Option<KeyCode> {
    Some(match vk {
        0x1B => KeyCode::KEY_ESC,
        0x31 => KeyCode::KEY_1,
        0x32 => KeyCode::KEY_2,
        0x33 => KeyCode::KEY_3,
        0x34 => KeyCode::KEY_4,
        0x35 => KeyCode::KEY_5,
        0x36 => KeyCode::KEY_6,
        0x37 => KeyCode::KEY_7,
        0x38 => KeyCode::KEY_8,
        0x39 => KeyCode::KEY_9,
        0x30 => KeyCode::KEY_0,
        0xBD => KeyCode::KEY_MINUS,
        0xBB => KeyCode::KEY_EQUAL,
        0x08 => KeyCode::KEY_BACKSPACE,
        0x09 => KeyCode::KEY_TAB,
        0x51 => KeyCode::KEY_Q,
        0x57 => KeyCode::KEY_W,
        0x45 => KeyCode::KEY_E,
        0x52 => KeyCode::KEY_R,
        0x54 => KeyCode::KEY_T,
        0x59 => KeyCode::KEY_Y,
        0x55 => KeyCode::KEY_U,
        0x49 => KeyCode::KEY_I,
        0x4F => KeyCode::KEY_O,
        0x50 => KeyCode::KEY_P,
        0xDB => KeyCode::KEY_LEFTBRACE,
        0xDD => KeyCode::KEY_RIGHTBRACE,
        0x0D => KeyCode::KEY_ENTER,
        0xA2 => KeyCode::KEY_LEFTCTRL,
        0x41 => KeyCode::KEY_A,
        0x53 => KeyCode::KEY_S,
        0x44 => KeyCode::KEY_D,
        0x46 => KeyCode::KEY_F,
        0x47 => KeyCode::KEY_G,
        0x48 => KeyCode::KEY_H,
        0x4A => KeyCode::KEY_J,
        0x4B => KeyCode::KEY_K,
        0x4C => KeyCode::KEY_L,
        0xBA => KeyCode::KEY_SEMICOLON,
        0xDE => KeyCode::KEY_APOSTROPHE,
        0xC0 => KeyCode::KEY_GRAVE,
        0xA0 => KeyCode::KEY_LEFTSHIFT,
        0xDC => KeyCode::KEY_BACKSLASH,
        0x5A => KeyCode::KEY_Z,
        0x58 => KeyCode::KEY_X,
        0x43 => KeyCode::KEY_C,
        0x56 => KeyCode::KEY_V,
        0x42 => KeyCode::KEY_B,
        0x4E => KeyCode::KEY_N,
        0x4D => KeyCode::KEY_M,
        0xBC => KeyCode::KEY_COMMA,
        0xBE => KeyCode::KEY_DOT,
        0xBF => KeyCode::KEY_SLASH,
        0xA1 => KeyCode::KEY_RIGHTSHIFT,
        0x6A => KeyCode::KEY_KPASTERISK,
        0xA4 => KeyCode::KEY_LEFTALT,
        0x20 => KeyCode::KEY_SPACE,
        0x14 => KeyCode::KEY_CAPSLOCK,
        0x70 => KeyCode::KEY_F1,
        0x71 => KeyCode::KEY_F2,
        0x72 => KeyCode::KEY_F3,
        0x73 => KeyCode::KEY_F4,
        0x74 => KeyCode::KEY_F5,
        0x75 => KeyCode::KEY_F6,
        0x76 => KeyCode::KEY_F7,
        0x77 => KeyCode::KEY_F8,
        0x78 => KeyCode::KEY_F9,
        0x79 => KeyCode::KEY_F10,
        0x7A => KeyCode::KEY_F11,
        0x7B => KeyCode::KEY_F12,
        0x90 => KeyCode::KEY_NUMLOCK,
        0x91 => KeyCode::KEY_SCROLLLOCK,
        0x67 => KeyCode::KEY_KP7,
        0x68 => KeyCode::KEY_KP8,
        0x69 => KeyCode::KEY_KP9,
        0x6D => KeyCode::KEY_KPMINUS,
        0x64 => KeyCode::KEY_KP4,
        0x65 => KeyCode::KEY_KP5,
        0x66 => KeyCode::KEY_KP6,
        0x6B => KeyCode::KEY_KPPLUS,
        0x61 => KeyCode::KEY_KP1,
        0x62 => KeyCode::KEY_KP2,
        0x63 => KeyCode::KEY_KP3,
        0x60 => KeyCode::KEY_KP0,
        0x6E => KeyCode::KEY_KPDOT,
        0x6C => KeyCode::KEY_KPENTER,
        0xA3 => KeyCode::KEY_RIGHTCTRL,
        0x6F => KeyCode::KEY_KPSLASH,
        0x2C => KeyCode::KEY_SYSRQ,
        0xA5 => KeyCode::KEY_RIGHTALT,
        0x24 => KeyCode::KEY_HOME,
        0x26 => KeyCode::KEY_UP,
        0x21 => KeyCode::KEY_PAGEUP,
        0x25 => KeyCode::KEY_LEFT,
        0x27 => KeyCode::KEY_RIGHT,
        0x23 => KeyCode::KEY_END,
        0x28 => KeyCode::KEY_DOWN,
        0x22 => KeyCode::KEY_PAGEDOWN,
        0x2D => KeyCode::KEY_INSERT,
        0x2E => KeyCode::KEY_DELETE,
        0x13 => KeyCode::KEY_PAUSE,
        0x5B => KeyCode::KEY_LEFTMETA,
        0x5C => KeyCode::KEY_RIGHTMETA,
        0xAD => KeyCode::KEY_MUTE,
        0xAE => KeyCode::KEY_VOLUMEDOWN,
        0xAF => KeyCode::KEY_VOLUMEUP,
        0x5E => KeyCode::KEY_POWER,
        0x5F => KeyCode::KEY_SLEEP,
        0xE3 => KeyCode::KEY_WAKEUP,
        0xB4 => KeyCode::KEY_MAIL,
        0xAB => KeyCode::KEY_BOOKMARKS,
        0xB5 => KeyCode::KEY_COMPUTER,
        0xA6 => KeyCode::KEY_BACK,
        0xA7 => KeyCode::KEY_FORWARD,
        0xB0 => KeyCode::KEY_NEXTSONG,
        0xB3 => KeyCode::KEY_PLAYPAUSE,
        0xB1 => KeyCode::KEY_PREVIOUSSONG,
        0xB2 => KeyCode::KEY_STOPCD,
        0xAC => KeyCode::KEY_HOMEPAGE,
        0xA8 => KeyCode::KEY_REFRESH,
        _ => return None,
    })
}
