use std::collections::VecDeque;

#[derive(Debug, PartialEq)]
pub struct Stamped<T> {
    pub seq: u64,
    pub t_ns: u64,
    pub value: T,
}

pub fn take_matched_within<T, U>(
    left: &mut VecDeque<Stamped<T>>,
    right: &mut VecDeque<Stamped<U>>,
    tol_ns: u64,
) -> Option<(Stamped<T>, Stamped<U>)> {
    loop {
        let (Some(l), Some(r)) = (left.front(), right.front()) else {
            return None;
        };
        let synced =
            l.seq == r.seq || (l.t_ns != 0 && r.t_ns != 0 && l.t_ns.abs_diff(r.t_ns) <= tol_ns);
        if synced {
            let l = left.pop_front().unwrap();
            let r = right.pop_front().unwrap();
            return Some((l, r));
        }
        let left_older = if l.t_ns != 0 && r.t_ns != 0 && l.t_ns != r.t_ns {
            l.t_ns < r.t_ns
        } else {
            l.seq < r.seq
        };
        if left_older {
            left.pop_front();
        } else {
            right.pop_front();
        }
    }
}

pub fn push_pending<T>(q: &mut VecDeque<Stamped<T>>, item: Stamped<T>, max: usize) {
    q.push_back(item);
    while q.len() > max {
        q.pop_front();
    }
}

#[cfg(test)]
pub fn take_matched<T, U>(
    left: &mut VecDeque<Stamped<T>>,
    depth: &mut VecDeque<Stamped<U>>,
) -> Option<(Stamped<T>, Stamped<U>)> {
    loop {
        let (Some(l), Some(d)) = (left.front(), depth.front()) else {
            return None;
        };
        if l.seq == d.seq || (l.t_ns != 0 && l.t_ns == d.t_ns) {
            let l = left.pop_front().unwrap();
            let d = depth.pop_front().unwrap();
            return Some((l, d));
        }
        let left_older = if l.t_ns != 0 && d.t_ns != 0 && l.t_ns != d.t_ns {
            l.t_ns < d.t_ns
        } else {
            l.seq < d.seq
        };
        if left_older {
            left.pop_front();
        } else {
            depth.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(seq: u64) -> Stamped<u64> {
        Stamped {
            seq,
            t_ns: seq * 1_000,
            value: seq,
        }
    }

    #[test]
    fn drops_left_that_has_no_depth() {
        let mut left = VecDeque::from([stamp(1), stamp(2), stamp(3)]);
        let mut depth = VecDeque::from([stamp(2), stamp(3)]);
        let (l, d) = take_matched(&mut left, &mut depth).unwrap();
        assert_eq!((l.value, d.value), (2, 2));
        let (l, d) = take_matched(&mut left, &mut depth).unwrap();
        assert_eq!((l.value, d.value), (3, 3));
        assert_eq!(take_matched(&mut left, &mut depth), None);
    }

    #[test]
    fn waits_until_both_sides_arrive() {
        let mut left = VecDeque::from([stamp(4)]);
        let mut depth = VecDeque::new();
        assert_eq!(take_matched(&mut left, &mut depth), None);
        depth.push_back(stamp(4));
        let (l, d) = take_matched(&mut left, &mut depth).unwrap();
        assert_eq!((l.value, d.value), (4, 4));
    }

    #[test]
    fn matches_equal_timestamps_when_sequences_differ() {
        let mut left = VecDeque::from([Stamped {
            seq: 1,
            t_ns: 50,
            value: 7,
        }]);
        let mut depth = VecDeque::from([Stamped {
            seq: 9,
            t_ns: 50,
            value: 8,
        }]);
        let (l, d) = take_matched(&mut left, &mut depth).unwrap();
        assert_eq!((l.value, d.value), (7, 8));
    }

    #[test]
    fn matches_timestamps_within_tolerance() {
        let mut left = VecDeque::from([Stamped {
            seq: 1,
            t_ns: 1_000,
            value: 1,
        }]);
        let mut right = VecDeque::from([Stamped {
            seq: 9,
            t_ns: 1_400,
            value: 2,
        }]);
        let (l, r) = take_matched_within(&mut left, &mut right, 500).unwrap();
        assert_eq!((l.value, r.value), (1, 2));
    }

    #[test]
    fn caps_the_pending_queue() {
        let mut q = VecDeque::new();
        push_pending(&mut q, stamp(1), 1);
        push_pending(&mut q, stamp(2), 1);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].seq, 2);
    }
}
