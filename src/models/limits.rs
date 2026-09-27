use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

// Per client: live protocol/queue data plus a separate retained UI history.
// Counts remain bounded too, so empty packets cannot exhaust metadata memory.
pub(crate) const MAX_PACKET_BYTES: usize = 256 * 1024;
pub(crate) const CLIENT_BUFFER_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const CONTROL_BUFFER_BYTES: usize = 512 * 1024;
pub(crate) const HISTORY_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_IN_FLIGHT: usize = 256;
pub(crate) const DETAIL_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct ByteBudget {
    used: Arc<AtomicUsize>,
    limit: usize,
}

impl ByteBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            used: Arc::new(AtomicUsize::new(0)),
            limit,
        }
    }

    pub(crate) fn reserve(&self, bytes: usize) -> Option<BytePermit> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|next| *next <= self.limit)
            })
            .ok()?;
        Some(BytePermit {
            budget: self.clone(),
            bytes,
        })
    }
}

#[derive(Debug)]
pub(crate) struct BytePermit {
    budget: ByteBudget,
    bytes: usize,
}

impl Drop for BytePermit {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_are_shared_and_released() {
        let budget = ByteBudget::new(10);
        let permit = budget.reserve(8).unwrap();
        assert!(budget.clone().reserve(3).is_none());
        assert!(budget.reserve(usize::MAX).is_none());
        drop(permit);
        assert!(budget.reserve(10).is_some());
    }
}
