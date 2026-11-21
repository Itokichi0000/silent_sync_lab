//! A scriptable [`ScanSource`] over the fixture chain and its fork, for tests
//! of the persistent sync: it can switch chains (reorg), hide its tail (a node
//! that is behind), fail after a number of fetches (interruption) and serve
//! blocks of one chain while claiming the tip of the other (race).

use crate::fixture_gen;
use std::cell::Cell;
use sync_core::{
    models::BlockRef,
    network::ChainIdentity,
    source::{fixture::FixtureSource, ScanBatch, ScanSource, SourceCapabilities, SourceError},
};

pub struct TestSource {
    main: FixtureSource,
    fork: FixtureSource,
    /// Which chain `tip` / `block_ref` describe.
    pub on_fork: Cell<bool>,
    /// Which chain `fetch_range` serves; `None` follows `on_fork`.
    pub fetch_from_fork: Cell<Option<bool>>,
    /// Pretend the chain ends at this height.
    pub visible_tip: Cell<Option<u32>>,
    /// Number of `fetch_range` calls that still succeed; `None` = unlimited.
    pub fetches_left: Cell<Option<u32>>,
    pub backend_id: String,
    pub chain_override: Cell<Option<ChainIdentity>>,
    pub fetch_calls: Cell<u32>,
    /// Serve batches that lack their last block (a source that silently
    /// returns less than was asked for).
    pub drop_last_block: Cell<bool>,
}

impl Default for TestSource {
    fn default() -> Self {
        Self::new()
    }
}

impl TestSource {
    pub fn new() -> Self {
        Self {
            main: FixtureSource::from_file(fixture_gen::generate()).expect("fixture"),
            fork: FixtureSource::from_file(fixture_gen::generate_fork()).expect("fork fixture"),
            on_fork: Cell::new(false),
            fetch_from_fork: Cell::new(None),
            visible_tip: Cell::new(None),
            fetches_left: Cell::new(None),
            backend_id: "fixture".to_string(),
            chain_override: Cell::new(None),
            fetch_calls: Cell::new(0),
            drop_last_block: Cell::new(false),
        }
    }

    pub fn main_chain(&self) -> &FixtureSource {
        &self.main
    }

    pub fn fork_chain(&self) -> &FixtureSource {
        &self.fork
    }

    fn described(&self) -> &FixtureSource {
        if self.on_fork.get() {
            &self.fork
        } else {
            &self.main
        }
    }

    fn served(&self) -> &FixtureSource {
        match self.fetch_from_fork.get() {
            Some(true) => &self.fork,
            Some(false) => &self.main,
            None => self.described(),
        }
    }

    fn limit(&self, real_tip: u32) -> u32 {
        self.visible_tip.get().map_or(real_tip, |v| v.min(real_tip))
    }
}

impl ScanSource for TestSource {
    fn backend_id(&self) -> &str {
        &self.backend_id
    }

    fn capabilities(&self) -> SourceCapabilities {
        self.main.capabilities()
    }

    fn chain(&self) -> ChainIdentity {
        self.chain_override
            .get()
            .unwrap_or_else(|| self.main.chain())
    }

    fn tip(&self) -> Result<BlockRef, SourceError> {
        let chain = self.described();
        let tip = self.limit(chain.tip()?.height);
        chain.block_ref(tip)
    }

    fn block_ref(&self, height: u32) -> Result<BlockRef, SourceError> {
        let chain = self.described();
        let tip = self.limit(chain.tip()?.height);
        if height > tip {
            return Err(SourceError::RangeUnavailable {
                from: height,
                to: height,
                available_from: 1,
                available_to: tip,
            });
        }
        chain.block_ref(height)
    }

    fn fetch_range(&self, from: u32, to: u32) -> Result<ScanBatch, SourceError> {
        self.fetch_calls.set(self.fetch_calls.get() + 1);
        if let Some(left) = self.fetches_left.get() {
            if left == 0 {
                return Err(SourceError::Unavailable("injected outage".to_string()));
            }
            self.fetches_left.set(Some(left - 1));
        }
        let chain = self.served();
        let tip = self.limit(chain.tip()?.height);
        if to > tip {
            return Err(SourceError::RangeUnavailable {
                from,
                to,
                available_from: 1,
                available_to: tip,
            });
        }
        let mut batch = chain.fetch_range(from, to)?;
        if self.drop_last_block.get() {
            batch.blocks.pop();
        }
        Ok(batch)
    }
}
