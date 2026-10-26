use serde::Serialize;

// 只等待有键盘抬起配对语义的实体键。通用修饰键别名、鼠标、IME 合成与保留 VK 不作为物理按住证据。
pub fn pairable(vk: u16) -> bool {
    matches!(vk,
        0x08..=0x09 | 0x0C..=0x0D | 0x13..=0x1F
        | 0x20..=0x2F | 0x30..=0x39 | 0x41..=0x5D | 0x5F
        | 0x60..=0x87 | 0x90..=0x96 | 0xA0..=0xB7
        | 0xBA..=0xC2 | 0xDB..=0xDF | 0xE1..=0xE4 | 0xE6
        | 0xE9..=0xFB | 0xFD..=0xFE)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    physical: usize,
    ime_synthetic: usize,
    aliases_or_mouse: usize,
    unsupported: usize,
}
impl Counts {
    pub fn record(&mut self, vk: u16) {
        let count = if pairable(vk) {
            &mut self.physical
        } else if matches!(vk, 0xE5 | 0xE7) {
            &mut self.ime_synthetic
        } else if matches!(vk, 0x01..=0x06 | 0x10..=0x12) {
            &mut self.aliases_or_mouse
        } else {
            &mut self.unsupported
        };
        *count = count.saturating_add(1);
    }
    pub fn sample(pressed: impl Fn(u16) -> bool) -> Self {
        let mut counts = Self::default();
        for vk in 0..=255 {
            if pressed(vk) {
                counts.record(vk);
            }
        }
        counts
    }
}

pub fn snapshot(swallowed: &[bool; 256], pressed: impl Fn(u16) -> bool) -> [bool; 256] {
    std::array::from_fn(|vk| pairable(vk as u16) && !swallowed[vk] && pressed(vk as u16))
}

pub fn track(passed: &mut [bool; 256], vk: u16, pressed: bool) {
    if let Some(slot) = passed.get_mut(vk as usize) {
        *slot = pairable(vk) && pressed;
    }
}

pub fn released(pressed: impl Fn(u16) -> bool) -> bool {
    !(0..=255).filter(|&vk| pairable(vk)).any(pressed)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recheck {
    pub released: usize,
    pub newly_pressed: usize,
}

// 只重采已放行的实体键；上下文前后任何一次不明确都不改原表，也不修改已吞键的配对证据。
pub fn recheck(
    passed: &mut [bool; 256],
    swallowed: &[bool; 256],
    readable_before: bool,
    pressed: impl Fn(u16) -> bool,
    readable_after: impl FnOnce() -> bool,
) -> Option<Recheck> {
    if !readable_before {
        return None;
    }
    let current = snapshot(swallowed, pressed);
    if !readable_after() {
        return None;
    }
    let mut result = Recheck::default();
    for (previous, current) in passed.iter_mut().zip(current) {
        if *previous && !current {
            result.released += 1;
        }
        if !*previous && current {
            result.newly_pressed += 1;
        }
        *previous = current;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_callback_and_error_fallback_share_the_same_pairing_boundary() {
        let swallowed = [false; 256];
        let synthetic = |vk| matches!(vk, 0xE5 | 0xE7 | 0xD8 | 0x10 | 0x11 | 0x01);
        assert!(!snapshot(&swallowed, synthetic).contains(&true));
        assert!(released(synthetic));
        let mut passed = [false; 256];
        for vk in [0xE5, 0xE7, 0xD8, 0x10, 0x11, 0x01] {
            track(&mut passed, vk, true);
        }
        assert!(!passed.contains(&true));
        track(&mut passed, 0x0D, true);
        track(&mut passed, 0xA2, true);
        assert!(passed[0x0D] && passed[0xA2]);
        assert!(!released(|vk| synthetic(vk) || vk == 0x0D));
        track(&mut passed, 0x0D, false);
        assert!(passed[0xA2], "Enter 松开不能跳过仍按着的左 Ctrl");
        track(&mut passed, 0xA2, false);
        assert!(!passed.contains(&true));
    }

    #[test]
    fn held_state_diagnostics_expose_counts_without_key_identity() {
        let counts = Counts::sample(|vk| matches!(vk, 0x0D | 0x41 | 0xE5 | 0xE7 | 0x11 | 0xD8));
        assert_eq!(
            serde_json::to_value(counts).unwrap(),
            serde_json::json!({
                "physical":2, "ime_synthetic":2, "aliases_or_mouse":1, "unsupported":1
            })
        );
        assert!(!pairable(256));
    }

    #[test]
    fn physical_ime_function_keys_and_oem_keys_keep_release_protection() {
        for vk in [0x15, 0x19, 0x1C, 0x1D, 0xBA, 0xDF, 0xE2] {
            let mut passed = snapshot(&[false; 256], |candidate| candidate == vk);
            assert!(passed[vk as usize] && !released(|candidate| candidate == vk));
            track(&mut passed, vk, false);
            assert!(
                !passed.contains(&true),
                "实体 IME 功能键和 OEM 键仍须等真正抬起"
            );
        }
    }

    #[test]
    fn zero_callback_rechecks_wait_for_enter_ctrl_and_new_direction_keys() {
        let swallowed = [false; 256];
        let mut passed = [false; 256];
        assert!(
            recheck(
                &mut passed,
                &swallowed,
                true,
                |vk| matches!(vk, 0x0D | 0xA2),
                || true
            )
            .is_some()
        );
        assert!(passed[0x0D] && passed[0xA2]);
        let changed = recheck(
            &mut passed,
            &swallowed,
            true,
            |vk| matches!(vk, 0xA2 | 0x28),
            || true,
        )
        .unwrap();
        assert_eq!(
            changed,
            Recheck {
                released: 1,
                newly_pressed: 1
            }
        );
        assert!(!passed[0x0D] && passed[0xA2] && passed[0x28]);
        recheck(&mut passed, &swallowed, true, |vk| vk == 0x28, || true);
        assert!(passed[0x28] && !passed[0xA2]);
        recheck(&mut passed, &swallowed, true, |_| false, || true);
        assert!(!passed.contains(&true));
    }

    #[test]
    fn uncertain_foreground_or_desktop_and_swallowed_keys_cannot_be_bypassed() {
        let mut swallowed = [false; 256];
        swallowed[0x41] = true;
        let original = swallowed;
        let mut passed = [false; 256];
        passed[0x0D] = true;
        assert!(recheck(&mut passed, &swallowed, false, |_| false, || true).is_none());
        assert!(passed[0x0D]);
        assert!(recheck(&mut passed, &swallowed, true, |_| false, || false).is_none());
        assert!(passed[0x0D]);
        recheck(&mut passed, &swallowed, true, |_| false, || true);
        assert_eq!(
            swallowed, original,
            "已吞 down 的历史证据完全不由物理轮询修改"
        );
        assert!(swallowed[0x41]);
    }
}
