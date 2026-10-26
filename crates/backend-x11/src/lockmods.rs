//! Lock-modifier expansion for hotkey grabs (pure; seg-05 §3).
//!
//! Grabbed chords must fire regardless of NumLock/CapsLock/ScrollLock, so
//! every chord is grabbed under all combinations of the *discovered* lock
//! masks. Masks are discovered from the server modifier map (never
//! hardcoded — `NumLock` is not always Mod2); the pure helpers below take
//! the discovered masks so T-504/T-510 run without a server.

/// Expand `base_modmask` over every combination of `lock_masks`.
///
/// Returns the powerset OR-ed onto the base (base included), zero masks
/// and duplicates removed, sorted. Two distinct locks yield four combos.
#[must_use]
pub fn expand_lock_combos(base_modmask: u32, lock_masks: &[u32]) -> Vec<u32> {
    let mut locks: Vec<u32> = lock_masks
        .iter()
        .copied()
        .filter(|mask| *mask != 0 && *mask != base_modmask)
        .collect();
    locks.sort_unstable();
    locks.dedup();
    let mut out = vec![base_modmask];
    for mask in locks {
        let current = out.clone();
        for combo in current {
            let expanded = combo | mask;
            if !out.contains(&expanded) {
                out.push(expanded);
            }
        }
    }
    out.sort_unstable();
    out
}

/// Build the discovered lock-mask list from abstracted modifier-map data.
///
/// The session layer feeds the real `GetModifierMapping` result here:
/// `numlock_mask`/`scrolllock_mask` are `None` when the corresponding key
/// sits on no modifier row, `capslock_mask` is the Lock row mask (usually
/// `1 << 1`). No mask value is ever hardcoded, so a `NumLock` on Mod5 (or
/// any other row) is honored (T-510).
#[must_use]
pub fn locks_from_map(
    numlock_mask: Option<u32>,
    capslock_mask: u32,
    scrolllock_mask: Option<u32>,
) -> Vec<u32> {
    let mut out = vec![capslock_mask];
    out.extend(numlock_mask);
    out.extend(scrolllock_mask);
    out.sort_unstable();
    out.dedup();
    out.retain(|mask| *mask != 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPS: u32 = 0x02;
    const NUM_MOD2: u32 = 0x10;
    const NUM_MOD5: u32 = 0x80;
    const SCROLL: u32 = 0x40;

    #[test]
    fn two_locks_yield_four_combos() {
        let combos = expand_lock_combos(0x04, &[CAPS, NUM_MOD2]);
        assert_eq!(combos, vec![0x04, 0x06, 0x14, 0x16]);
    }

    #[test]
    fn no_locks_yield_base_only() {
        assert_eq!(expand_lock_combos(0x04, &[]), vec![0x04]);
    }

    #[test]
    fn duplicate_and_zero_masks_dedup() {
        let combos = expand_lock_combos(0x04, &[CAPS, CAPS, 0, NUM_MOD2, NUM_MOD2]);
        assert_eq!(combos, vec![0x04, 0x06, 0x14, 0x16]);
    }

    #[test]
    fn non_mod2_numlock_is_honored() {
        // T-510: NumLock discovered on Mod5 must expand on Mod5, not Mod2.
        let locks = locks_from_map(Some(NUM_MOD5), CAPS, None);
        assert!(locks.contains(&NUM_MOD5));
        assert!(!locks.contains(&NUM_MOD2));
        let combos = expand_lock_combos(0, &locks);
        assert_eq!(combos.len(), 4);
        assert!(combos.contains(&(NUM_MOD5 | CAPS)));
    }

    #[test]
    fn three_locks_yield_eight_combos() {
        let locks = locks_from_map(Some(NUM_MOD2), CAPS, Some(SCROLL));
        assert_eq!(locks.len(), 3);
        assert_eq!(expand_lock_combos(0, &locks).len(), 8);
    }
}
