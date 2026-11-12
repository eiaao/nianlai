use super::paste::INPUT_MARKER;
use crate::platform::Result;
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use windows::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{Input::KeyboardAndMouse::GetAsyncKeyState, WindowsAndMessaging::*},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueEvent {
    Paste,
    Cancel,
    Timeout,
}

#[derive(Default)]
struct Pairing {
    v: bool,
    escape: bool,
}

// 钩子内自行跟踪的 Ctrl 按住状态。paste::execute 注入的合成 Ctrl（带 INPUT_MARKER）会把系统
// 异步键盘状态清掉：用户物理按住 Ctrl 时 GetAsyncKeyState 误报没按，第二个 V 就被当成
// 不带 Ctrl 的普通按键放行、打进了目标文档。所以这里只信非 marker 的 LCONTROL/RCONTROL 物理
// 按下/抬起；安装时用 GetAsyncKeyState 初始化一次，覆盖装钩子前就已经按住的情况。
#[derive(Default)]
struct Ctrl {
    left: bool,
    right: bool,
}
impl Ctrl {
    fn held(&self) -> bool {
        self.left || self.right
    }
    fn observe(&mut self, vk: u16, pressed: bool) {
        match vk {
            0xA2 => self.left = pressed,
            0xA3 => self.right = pressed,
            _ => {}
        }
    }
    fn from_async() -> Self {
        Self {
            left: down(0xA2),
            right: down(0xA3),
        }
    }
}

// 只接管逐张粘贴期间明确属于队列的键。Ctrl+V 在抬起时通知，整个 down/up 对都吞掉；
// Esc 同样成对吞掉、抬起时取消（不让 Excel 等在编辑态按 Esc 撤销刚粘贴的内容；有空闲超时，
// 不会长期劫持）；
// Tab、其它 Ctrl 组合、呼出键和普通编辑键一律放行。合成粘贴带 INPUT_MARKER，在更前面直接放行，
// 也不改动下面的本地 Ctrl 跟踪。
fn route(
    pairing: &mut Pairing,
    vk: u16,
    pressed: bool,
    control: bool,
    other_modifier: bool,
) -> (bool, Option<QueueEvent>) {
    if vk == 0x56 {
        if pairing.v {
            if !pressed {
                pairing.v = false;
                return (true, Some(QueueEvent::Paste));
            }
            return (true, None);
        }
        if pressed && control && !other_modifier {
            pairing.v = true;
            return (true, None);
        }
    }
    if vk == 0x1B {
        if pairing.escape {
            if !pressed {
                pairing.escape = false;
                return (true, Some(QueueEvent::Cancel));
            }
            return (true, None);
        }
        if pressed && !control && !other_modifier {
            pairing.escape = true;
            return (true, None);
        }
    }
    (false, None)
}

// route 状态机的完整驱动（纯函数，与 keyboard() 同一顺序）：物理事件先维护本地 Ctrl 跟踪，
// 再决定吞/放。注入的 marker 事件永远到不了这里。
fn physical(
    pairing: &mut Pairing,
    ctrl: &mut Ctrl,
    vk: u16,
    pressed: bool,
    other_modifier: bool,
) -> (bool, Option<QueueEvent>) {
    ctrl.observe(vk, pressed);
    route(pairing, vk, pressed, ctrl.held(), other_modifier)
}

struct KeyInput {
    vk: u16,
    pressed: bool,
    extended: bool,
    injected: bool,
    marker: bool,
    other_modifier: bool,
}

fn route_input(
    pairing: &mut Pairing,
    ctrl: &mut Ctrl,
    key: KeyInput,
    active: bool,
) -> (bool, Option<QueueEvent>) {
    // 所有注入事件均放行：其他程序的合成键也不能冒充真实 Ctrl，或结束物理 V/Esc 配对。
    if key.injected || key.marker {
        return (false, None);
    }
    let vk = if key.vk == 0x11 {
        if key.extended { 0xA3 } else { 0xA2 }
    } else {
        key.vk
    };
    if !active {
        ctrl.observe(vk, key.pressed);
        // 已吞下的按下必须连同抬起一起吞掉，不能把孤立的抬起漏给目标；新按下一律放行。
        let finishing =
            !key.pressed && ((vk == 0x56 && pairing.v) || (vk == 0x1B && pairing.escape));
        if key.pressed {
            *pairing = Pairing::default();
        } else if vk == 0x56 {
            pairing.v = false;
        } else if vk == 0x1B {
            pairing.escape = false;
        }
        return (finishing, None);
    }
    physical(pairing, ctrl, vk, key.pressed, key.other_modifier)
}

struct Context {
    callback: Box<dyn Fn(QueueEvent)>,
    pairing: Pairing,
    ctrl: Ctrl,
    enabled: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}
thread_local! { static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) }; }

fn down(vk: i32) -> bool {
    unsafe { GetAsyncKeyState(vk) < 0 }
}

unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    let pressed = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    let other = down(0x10)
        || down(0x12)
        || down(0x5B)
        || down(0x5C)
        || down(0xA0)
        || down(0xA1)
        || down(0xA4)
        || down(0xA5);
    let event = CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let context = context.as_mut()?;
        let active =
            context.enabled.load(Ordering::Acquire) && !context.cancelled.load(Ordering::Acquire);
        let (swallow, event) = route_input(
            &mut context.pairing,
            &mut context.ctrl,
            KeyInput {
                vk: key.vkCode as u16,
                pressed,
                extended: key.flags.contains(LLKHF_EXTENDED),
                injected: key.flags.contains(LLKHF_INJECTED)
                    || key.flags.contains(LLKHF_LOWER_IL_INJECTED),
                marker: key.dwExtraInfo == INPUT_MARKER,
                other_modifier: other,
            },
            active,
        );
        if let Some(event) = event {
            if event == QueueEvent::Cancel {
                context.cancelled.store(true, Ordering::Release);
            }
            (context.callback)(event);
        }
        swallow.then_some(())
    });
    if event.is_some() {
        LRESULT(1)
    } else {
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}

pub struct QueueHooks {
    id: u32,
    thread: Option<JoinHandle<()>>,
    enabled: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

impl QueueHooks {
    /// `expired` 在钩子线程的 50ms 计时器里调用（不经过主线程）：主线程卡住时也能自己
    /// 停止接管 Ctrl+V，避免空闲超时后仍吞键。
    pub fn install(
        cancelled: Arc<AtomicBool>,
        expired: impl Fn() -> bool + Send + 'static,
        callback: impl Fn(QueueEvent) + Send + 'static,
    ) -> Result<Self> {
        let (ready, receive_ready) = std::sync::mpsc::sync_channel(1);
        let enabled = Arc::new(AtomicBool::new(false));
        let thread_enabled = enabled.clone();
        let thread_cancelled = cancelled.clone();
        let thread = thread::Builder::new()
            .name("queue-hooks".into())
            .spawn(move || unsafe {
                let mut message = MSG::default();
                let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
                let module = GetModuleHandleW(None)
                    .ok()
                    .map(|module| HINSTANCE(module.0));
                CONTEXT.with(|context| {
                    *context.borrow_mut() = Some(Context {
                        callback: Box::new(callback),
                        pairing: Pairing::default(),
                        ctrl: Ctrl::from_async(),
                        enabled: thread_enabled,
                        cancelled: thread_cancelled.clone(),
                    })
                });
                let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0).ok();
                if hook.is_some() {
                    // 安装之后再采样一次，覆盖 Ctrl 早于钩子按下的情况。
                    CONTEXT.with(|context| {
                        if let Some(context) = context.borrow_mut().as_mut() {
                            context.ctrl = Ctrl::from_async();
                        }
                    });
                    let timer = SetTimer(None, 0, 50, None);
                    if timer == 0 {
                        let _ = ready.send(Err("逐张粘贴超时监控安装失败".to_owned()));
                        if let Some(hook) = hook {
                            let _ = UnhookWindowsHookEx(hook);
                        }
                        CONTEXT.with(|context| *context.borrow_mut() = None);
                        return;
                    }
                    let _ = ready.send(Ok(GetCurrentThreadId()));
                    loop {
                        let result = GetMessageW(&mut message, None, 0, 0).0;
                        if result <= 0 {
                            break;
                        }
                        if thread_cancelled.load(Ordering::Acquire) {
                            break;
                        }
                        if expired() {
                            thread_cancelled.store(true, Ordering::Release);
                            CONTEXT.with(|context| {
                                if let Some(context) = context.borrow().as_ref() {
                                    (context.callback)(QueueEvent::Timeout);
                                }
                            });
                            break;
                        }
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                    let _ = KillTimer(None, timer);
                } else {
                    let _ = ready.send(Err("逐张粘贴键盘钩子安装失败".to_owned()));
                }
                if let Some(hook) = hook {
                    let _ = UnhookWindowsHookEx(hook);
                }
                CONTEXT.with(|context| *context.borrow_mut() = None);
            })
            .map_err(|error| error.to_string())?;
        let installed = receive_ready.recv_timeout(Duration::from_secs(2));
        match installed {
            Ok(Ok(id)) => Ok(Self {
                id,
                thread: Some(thread),
                enabled,
                cancelled,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(error) => {
                cancelled.store(true, Ordering::Release);
                let _ = thread.join();
                Err(format!("逐张粘贴键盘监听未就绪：{error}"))
            }
        }
    }

    pub fn activate(&self) {
        self.enabled.store(true, Ordering::Release);
    }
}

impl Drop for QueueHooks {
    fn drop(&mut self) {
        self.enabled.store(false, Ordering::Release);
        self.cancelled.store(true, Ordering::Release);
        unsafe {
            let _ = PostThreadMessageW(self.id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_v_is_paired_and_fires_once_on_release() {
        let mut pairing = Pairing::default();
        assert_eq!(route(&mut pairing, 0x56, true, true, false), (true, None));
        assert_eq!(route(&mut pairing, 0x56, true, true, false), (true, None));
        assert_eq!(
            route(&mut pairing, 0x56, true, false, false),
            (true, None),
            "重复按下仍按最初配对吞掉"
        );
        assert_eq!(
            route(&mut pairing, 0x56, false, false, false),
            (true, Some(QueueEvent::Paste))
        );
        assert_eq!(
            route(&mut pairing, 0x56, false, true, false),
            (false, None),
            "新抬起不重复触发"
        );
    }

    #[test]
    fn modified_v_and_all_unrelated_keys_pass() {
        let mut pairing = Pairing::default();
        for (control, other) in [(false, false), (true, true), (false, true)] {
            assert_eq!(
                route(&mut pairing, 0x56, true, control, other),
                (false, None)
            );
            assert_eq!(
                route(&mut pairing, 0x56, false, control, other),
                (false, None)
            );
        }
        for vk in [0x09, 0x0D, 0x41, 0xC0, 0x25, 0x26, 0x27, 0x28] {
            assert_eq!(route(&mut pairing, vk, true, true, false), (false, None));
        }
    }

    // 问题 4：Esc 成对吞掉，抬起时取消；带修饰键的 Esc 放行。
    #[test]
    fn escape_is_swallowed_in_pairs_and_cancels_on_release() {
        let mut pairing = Pairing::default();
        assert_eq!(route(&mut pairing, 0x1B, true, false, false), (true, None));
        assert_eq!(
            route(&mut pairing, 0x1B, true, false, false),
            (true, None),
            "长按的重复按下照样吞"
        );
        assert_eq!(
            route(&mut pairing, 0x1B, false, false, false),
            (true, Some(QueueEvent::Cancel))
        );
        assert_eq!(
            route(&mut pairing, 0x1B, false, false, false),
            (false, None)
        );
        assert_eq!(
            route(&mut pairing, 0x1B, true, true, false),
            (false, None),
            "Ctrl+Esc 放行"
        );
        assert_eq!(
            route(&mut pairing, 0x1B, true, false, true),
            (false, None),
            "Shift/Alt/Win+Esc 放行"
        );
    }

    // 取消（或停止）后钩子失效，但已吞下的 Esc / V 的抬起仍要吞掉。
    #[test]
    fn swallowed_presses_keep_their_release_after_deactivation() {
        let mut pairing = Pairing::default();
        let mut ctrl = Ctrl::default();
        assert_eq!(
            route_input(&mut pairing, &mut ctrl, key(0x1B, true), true),
            (true, None)
        );
        assert_eq!(
            route_input(&mut pairing, &mut ctrl, key(0x1B, false), false),
            (true, None)
        );
        assert_eq!(
            route_input(&mut pairing, &mut ctrl, key(0x1B, false), false),
            (false, None)
        );
        ctrl.left = true;
        assert_eq!(
            route_input(&mut pairing, &mut ctrl, key(0x56, true), true),
            (true, None)
        );
        assert_eq!(
            route_input(&mut pairing, &mut ctrl, key(0x56, false), false),
            (true, None)
        );
        assert_eq!(
            route_input(&mut pairing, &mut ctrl, key(0x56, true), false),
            (false, None)
        );
    }

    // 问题 1 的完整场景：按住 Ctrl 依次按 V、V、V，中间夹着每张粘贴注入的 marker 键
    // （preflight 的 Ctrl 抬起和 Ctrl+V 序列）。三下 V 各得一次 Paste，全部被吞，不漏给目标。
    #[test]
    fn holding_ctrl_and_tapping_v_three_times_pastes_three_times() {
        let mut pairing = Pairing::default();
        let mut ctrl = Ctrl::default();
        let mut events = Vec::new();
        let mut leaked = Vec::new();
        let mut feed = |pairing: &mut Pairing, ctrl: &mut Ctrl, input: KeyInput| {
            let vk = input.vk;
            let pressed = input.pressed;
            let (swallow, event) = route_input(pairing, ctrl, input, true);
            if let Some(event) = event {
                events.push(event);
            }
            if !swallow && vk == 0x56 {
                leaked.push(pressed);
            }
        };
        let injected = |vk: u16, pressed: bool| KeyInput {
            marker: true,
            injected: true,
            ..key(vk, pressed)
        };
        feed(&mut pairing, &mut ctrl, key(0xA2, true));
        for _ in 0..3 {
            feed(&mut pairing, &mut ctrl, key(0x56, true));
            feed(&mut pairing, &mut ctrl, key(0x56, false));
            // 这一张的 paste::execute：补抬起 Ctrl，再发 Ctrl+V。
            for (vk, pressed) in [
                (0xA2, false),
                (0x11, true),
                (0x56, true),
                (0x56, false),
                (0x11, false),
            ] {
                feed(&mut pairing, &mut ctrl, injected(vk, pressed));
            }
        }
        feed(&mut pairing, &mut ctrl, key(0xA2, false));
        assert_eq!(events, [QueueEvent::Paste; 3]);
        // 注入的 V（marker）放行属于预期；物理 V 一个都不能漏。
        assert_eq!(leaked.len(), 6, "只有 3 组注入的 V 放行");
        assert!(!ctrl.held());
    }

    // 审查问题 1：第一张粘贴由 paste::execute 注入的 marker Ctrl 抬起会清掉异步键盘状态，
    // 但用户物理按住 Ctrl 时第二张的 V 仍必须按 Paste 吞下，直到物理 Ctrl 抬起。
    #[test]
    fn injected_marker_ctrl_release_does_not_release_locally_tracked_control() {
        let mut pairing = Pairing::default();
        let mut ctrl = Ctrl::default();
        assert_eq!(
            physical(&mut pairing, &mut ctrl, 0xA2, true, false),
            (false, None),
            "物理 Ctrl 按下放行"
        );
        assert_eq!(
            physical(&mut pairing, &mut ctrl, 0x56, true, false),
            (true, None)
        );
        assert_eq!(
            physical(&mut pairing, &mut ctrl, 0x56, false, false),
            (true, Some(QueueEvent::Paste))
        );
        // 此处 paste::execute 注入了带 INPUT_MARKER 的 Ctrl 抬起：keyboard() 对 marker 直接放行，
        // 状态机看不到，本地 control 仍是按住。
        assert!(ctrl.held(), "注入的 marker Ctrl 抬起不能清掉本地跟踪");
        assert_eq!(
            physical(&mut pairing, &mut ctrl, 0x56, true, false),
            (true, None)
        );
        assert_eq!(
            physical(&mut pairing, &mut ctrl, 0x56, false, false),
            (true, Some(QueueEvent::Paste)),
            "物理 Ctrl 仍按住，第二张的 V 仍得 Paste 且被吞"
        );
        assert_eq!(
            physical(&mut pairing, &mut ctrl, 0xA2, false, false),
            (false, None),
            "物理 Ctrl 抬起放行"
        );
        assert_eq!(
            physical(&mut pairing, &mut ctrl, 0x56, true, false),
            (false, None)
        );
        assert_eq!(
            physical(&mut pairing, &mut ctrl, 0x56, false, false),
            (false, None),
            "松开 Ctrl 后的 V 放行给目标"
        );
    }

    // 左右 Ctrl 各自跟踪：松开一边另一边仍算按住；安装时已按住的（from_async 初始化）同样生效。
    #[test]
    fn either_control_side_keeps_the_chord() {
        let mut pairing = Pairing::default();
        let mut ctrl = Ctrl {
            left: true,
            right: false,
        };
        assert!(ctrl.held());
        physical(&mut pairing, &mut ctrl, 0xA3, true, false);
        assert_eq!(
            physical(&mut pairing, &mut ctrl, 0xA2, false, false),
            (false, None),
            "松开左 Ctrl（放行），右 Ctrl 仍按住"
        );
        assert!(ctrl.held());
        assert_eq!(
            physical(&mut pairing, &mut ctrl, 0x56, true, false),
            (true, None)
        );
        assert_eq!(
            physical(&mut pairing, &mut ctrl, 0x56, false, false),
            (true, Some(QueueEvent::Paste))
        );
        physical(&mut pairing, &mut ctrl, 0xA3, false, false);
        assert!(!ctrl.held());
        assert_eq!(
            physical(&mut pairing, &mut ctrl, 0x56, true, false),
            (false, None),
            "两边都松开后 V 放行"
        );
    }

    fn key(vk: u16, pressed: bool) -> KeyInput {
        KeyInput {
            vk,
            pressed,
            extended: false,
            injected: false,
            marker: false,
            other_modifier: false,
        }
    }

    #[test]
    fn actual_injected_and_marker_events_cannot_change_physical_ctrl_or_pairing() {
        for (injected, marker) in [(true, false), (true, true), (false, true)] {
            let mut pairing = Pairing::default();
            // 安装时 Ctrl 已经按住。
            let mut ctrl = Ctrl {
                left: true,
                right: false,
            };
            assert_eq!(
                route_input(&mut pairing, &mut ctrl, key(0x56, true), true),
                (true, None)
            );
            for vk in [0xA2, 0xA3, 0x11, 0x56, 0x1B] {
                let mut event = key(vk, false);
                event.injected = injected;
                event.marker = marker;
                assert_eq!(
                    route_input(&mut pairing, &mut ctrl, event, true),
                    (false, None)
                );
            }
            assert!(ctrl.held());
            assert_eq!(
                route_input(&mut pairing, &mut ctrl, key(0x56, false), true),
                (true, Some(QueueEvent::Paste))
            );
            assert_eq!(
                route_input(&mut pairing, &mut ctrl, key(0x56, true), true),
                (true, None)
            );
            // 乱序：物理 Ctrl 先抬起，V 的抬起仍属于队列。
            route_input(&mut pairing, &mut ctrl, key(0xA2, false), true);
            assert_eq!(
                route_input(&mut pairing, &mut ctrl, key(0x56, false), true),
                (true, Some(QueueEvent::Paste))
            );
            assert_eq!(
                route_input(&mut pairing, &mut ctrl, key(0x56, false), true),
                (false, None)
            );
            assert_eq!(
                route_input(&mut pairing, &mut ctrl, key(0x56, true), true),
                (false, None)
            );
        }
    }

    #[test]
    fn injected_ctrl_down_never_creates_a_physical_chord() {
        let mut pairing = Pairing::default();
        let mut ctrl = Ctrl::default();
        let mut event = key(0xA2, true);
        event.injected = true;
        route_input(&mut pairing, &mut ctrl, event, true);
        assert!(!ctrl.held());
        assert_eq!(
            route_input(&mut pairing, &mut ctrl, key(0x56, true), true),
            (false, None)
        );
        let mut right = key(0x11, true);
        right.extended = true;
        route_input(&mut pairing, &mut ctrl, right, true);
        route_input(&mut pairing, &mut ctrl, key(0xA2, false), true);
        assert!(ctrl.right);
        assert_eq!(
            route_input(&mut pairing, &mut ctrl, key(0x56, true), true),
            (true, None)
        );
    }

    #[test]
    fn escape_and_ctrl_v_pass_during_installation_and_after_stop() {
        let mut pairing = Pairing::default();
        let mut ctrl = Ctrl::default();
        for vk in [0x1B, 0xA2, 0x56] {
            for pressed in [true, false] {
                assert_eq!(
                    route_input(&mut pairing, &mut ctrl, key(vk, pressed), false),
                    (false, None)
                );
            }
        }
        assert_eq!(
            route_input(&mut pairing, &mut ctrl, key(0x1B, true), true),
            (true, None)
        );
        assert_eq!(
            route_input(&mut pairing, &mut ctrl, key(0x1B, false), true),
            (true, Some(QueueEvent::Cancel))
        );
        assert_eq!(
            route_input(&mut pairing, &mut ctrl, key(0x1B, true), false),
            (false, None)
        );
        assert_eq!(
            route_input(&mut pairing, &mut ctrl, key(0x1B, false), false),
            (false, None)
        );
    }
}
