/// Multi-path scheduler: decides which path carries each symbol.
///
/// A transfer has up to N edge relays plus one optional P2P direct channel.
/// Symbols are interchangeable (rateless fountain), so any deterministic
/// spread is valid; the scheduler keeps the requested relay/direct ratio
/// while round-robining the relay load across the N relays.
pub struct MultiPathScheduler {
    relay_ratio: f32,
    n_relays: usize,
    next_relay: usize,
}

impl MultiPathScheduler {
    pub fn new(p2p_bw: u64, relay_bw: u64) -> Self {
        let total = p2p_bw + relay_bw;
        let relay_ratio = if total == 0 {
            0.5
        } else {
            relay_bw as f32 / total as f32
        };
        Self {
            relay_ratio,
            n_relays: 1,
            next_relay: 0,
        }
    }

    /// Sets the number of edge relays the sender is connected to.
    pub fn with_relays(mut self, n: usize) -> Self {
        self.n_relays = n.max(1);
        self
    }

    /// For symbol `seq`, returns the relay index that should carry it, or
    /// `None` when it should go over the P2P direct channel.
    ///
    /// The relay load is round-robined over the N relays with an
    /// independent counter, so the relay/direct ratio never correlates with
    /// the relay index (e.g. `ratio = 0.5` and `n = 2` must not starve one
    /// relay).
    pub fn pick(&mut self, seq: u32) -> Option<usize> {
        let slot = (seq as f32 * self.relay_ratio).fract();
        if slot < self.relay_ratio {
            let i = self.next_relay;
            self.next_relay = (self.next_relay + 1) % self.n_relays;
            Some(i)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pure_p2p() {
        let mut s = MultiPathScheduler::new(1000, 0);
        assert_eq!(s.pick(0), None);
        assert_eq!(s.pick(42), None);
    }

    #[test]
    fn test_pure_relay() {
        let mut s = MultiPathScheduler::new(0, 1000);
        assert_eq!(s.pick(0), Some(0));
        assert_eq!(s.pick(7), Some(0));
    }

    #[test]
    fn test_balanced_split() {
        let mut s = MultiPathScheduler::new(1, 1);
        let mut direct = 0;
        let mut relay = 0;
        for seq in 0..1000u32 {
            match s.pick(seq) {
                Some(_) => relay += 1,
                None => direct += 1,
            }
        }
        assert!((relay as i32 - direct as i32).abs() <= 1, "{relay} vs {direct}");
    }

    #[test]
    fn test_multi_relay_spread() {
        let mut s = MultiPathScheduler::new(1, 1).with_relays(3);
        let mut used = std::collections::HashSet::new();
        for seq in 0..1000u32 {
            if let Some(i) = s.pick(seq) {
                used.insert(i);
            }
        }
        assert_eq!(used.len(), 3, "every relay must carry at least one symbol");
    }

    #[test]
    fn test_ratio_does_not_starve_a_relay() {
        // ratio 0.5 + 2 relays: half of the symbols go to relays, spread
        // evenly across both (the old `seq % n` formula starved relay 1).
        let mut s = MultiPathScheduler::new(1, 1).with_relays(2);
        let mut per_relay = [0usize; 2];
        for seq in 0..1000u32 {
            if let Some(i) = s.pick(seq) {
                per_relay[i] += 1;
            }
        }
        assert!(per_relay[0] > 0 && per_relay[1] > 0, "{per_relay:?}");
        assert!(
            (per_relay[0] as i32 - per_relay[1] as i32).abs() <= 1,
            "relays must share the load evenly: {per_relay:?}"
        );
    }
}