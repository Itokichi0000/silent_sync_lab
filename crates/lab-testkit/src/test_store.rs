//! Store wrappers for tests: fault injection that does not belong in a
//! production store.

use silent_sync::{
    models::{BlockRef, DetectedOutput, SpentOutput},
    store::{StoreError, StoreMeta, SyncStore},
};

/// Wraps any store and makes its commits fail on demand, the way a crash
/// before commit would: a failing `append` changes nothing.
pub struct FailingAppends<S> {
    pub inner: S,
    /// When `Some(n)`, the next `n` appends succeed and every later one fails.
    appends_before_failure: Option<u32>,
}

impl<S: SyncStore> FailingAppends<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            appends_before_failure: None,
        }
    }

    /// The next `n` appends succeed; every later one fails atomically.
    pub fn fail_appends_after(&mut self, n: u32) {
        self.appends_before_failure = Some(n);
    }

    /// Lets appends succeed again.
    pub fn stop_failing(&mut self) {
        self.appends_before_failure = None;
    }
}

impl<S: SyncStore> SyncStore for FailingAppends<S> {
    fn meta(&self) -> Result<Option<StoreMeta>, StoreError> {
        self.inner.meta()
    }
    fn initialize(&mut self, meta: &StoreMeta) -> Result<(), StoreError> {
        self.inner.initialize(meta)
    }
    fn checkpoint(&self) -> Result<Option<BlockRef>, StoreError> {
        self.inner.checkpoint()
    }
    fn block_at(&self, height: u32) -> Result<Option<BlockRef>, StoreError> {
        self.inner.block_at(height)
    }
    fn append(
        &mut self,
        blocks: &[BlockRef],
        detections: &[DetectedOutput],
        spends: &[SpentOutput],
    ) -> Result<(), StoreError> {
        if let Some(remaining) = self.appends_before_failure.as_mut() {
            if *remaining == 0 {
                return Err(StoreError::Backend("injected failure".to_string()));
            }
            *remaining -= 1;
        }
        self.inner.append(blocks, detections, spends)
    }
    fn rollback_to(&mut self, height: u32) -> Result<Vec<DetectedOutput>, StoreError> {
        self.inner.rollback_to(height)
    }
    fn detections(&self) -> Result<Vec<DetectedOutput>, StoreError> {
        self.inner.detections()
    }
    fn spends(&self) -> Result<Vec<SpentOutput>, StoreError> {
        self.inner.spends()
    }
    fn block_count(&self) -> Result<u64, StoreError> {
        self.inner.block_count()
    }
}
