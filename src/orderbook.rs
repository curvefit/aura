use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use crate::{AuraError, Result};

const SIDE_COUNT: usize = 256;
const MISSING_INDEX: usize = usize::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookStateHash(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BookSide {
    Bid,
    Ask,
    Other(u8),
}

impl BookSide {
    pub fn try_from_i64(value: i64) -> Result<Self> {
        let raw = u8::try_from(value).map_err(|_| AuraError::InvalidValue("book side"))?;
        Ok(match raw {
            1 => Self::Bid,
            2 => Self::Ask,
            other => Self::Other(other),
        })
    }

    pub const fn raw(self) -> u8 {
        match self {
            Self::Bid => 1,
            Self::Ask => 2,
            Self::Other(raw) => raw,
        }
    }

    pub const fn raw_i64(self) -> i64 {
        self.raw() as i64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrderBookOperation {
    Set,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderBookDelta {
    pub timestamp: i64,
    pub instrument: i64,
    pub side: BookSide,
    pub price: i64,
    pub size: i64,
    pub flags: i64,
    pub action: i64,
    pub sequence: i64,
    pub order_id: i64,
    operation: OrderBookOperation,
}

impl OrderBookDelta {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        timestamp: i64,
        instrument: i64,
        side: i64,
        price: i64,
        size: i64,
        flags: i64,
        action: i64,
        sequence: i64,
        order_id: i64,
    ) -> Result<Self> {
        let side = BookSide::try_from_i64(side)?;
        let operation = if size <= 0 || action == 2 {
            OrderBookOperation::Remove
        } else {
            OrderBookOperation::Set
        };
        Ok(Self {
            timestamp,
            instrument,
            side,
            price,
            size,
            flags,
            action,
            sequence,
            order_id,
            operation,
        })
    }

    pub const fn is_remove(&self) -> bool {
        matches!(self.operation, OrderBookOperation::Remove)
    }

    pub const fn is_zero_size_remove(&self) -> bool {
        self.size <= 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BookLevel {
    pub instrument: i64,
    pub side: BookSide,
    pub price: i64,
    pub size: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderBookApplyMode {
    Production,
    Verify,
}

impl OrderBookApplyMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Verify => "verify",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderBookLifecycleMode {
    Cold,
    Prepared,
}

impl OrderBookLifecycleMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Prepared => "prepared",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderBookEngineKind {
    Current,
    PackedKey,
    DenseLadder,
    PagedLadder { page_size: usize },
    DirectIndex,
    RunLocality,
    BTreeMap,
    Optimized,
}

impl OrderBookEngineKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::PackedKey => "packed-key",
            Self::DenseLadder => "dense-ladder",
            Self::PagedLadder { .. } => "paged-ladder",
            Self::DirectIndex => "direct-index",
            Self::RunLocality => "run-locality",
            Self::BTreeMap => "btree-map",
            Self::Optimized => "optimized",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OrderBookApplyPlan {
    pub kind: OrderBookEngineKind,
    record_count: usize,
    instruments: Vec<i64>,
    instrument_min: i64,
    instrument_lookup: Vec<usize>,
    side_lookup: Vec<usize>,
    side_plans: Vec<SidePlan>,
    price_levels: usize,
    total_slots: usize,
}

#[derive(Debug, Clone, Copy)]
struct SidePlan {
    instrument: i64,
    side: BookSide,
    min_price: i64,
    max_price: i64,
    base_offset: usize,
}

impl SidePlan {
    fn len(self) -> usize {
        usize::try_from(self.max_price - self.min_price + 1).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BookApplyBreakdown {
    pub setup_ns: u128,
    pub plan_build_ns: u128,
    pub instrument_lookup_ns: u128,
    pub side_lookup_ns: u128,
    pub price_lookup_ns: u128,
    pub update_remove_ns: u128,
    pub allocation_ns: u128,
    pub reset_ns: u128,
    pub state_hash_ns: u128,
    pub loop_overhead_ns: u128,
}

#[derive(Debug, Clone)]
pub struct BookApplyStats {
    pub apply_mode: OrderBookApplyMode,
    pub lifecycle_mode: OrderBookLifecycleMode,
    pub engine_kind: &'static str,
    pub book_levels: usize,
    pub instruments: usize,
    pub price_levels: usize,
    pub adds: usize,
    pub modifies: usize,
    pub deletes: usize,
    pub zero_size_removes: usize,
    pub state_hash: BookStateHash,
    pub allocation_count_proxy: usize,
    pub bytes_allocated_proxy: usize,
    pub cache_shape: &'static str,
    pub breakdown: BookApplyBreakdown,
    pub state_hash_checked: bool,
    pub engine_reused: bool,
    pub buffers_reused: bool,
}

#[derive(Debug)]
pub struct OrderBookEngine {
    plan: OrderBookApplyPlan,
    storage: EngineStorage,
    stats: MutableStats,
}

#[derive(Debug, Clone)]
pub struct PreparedOrderBookApplyPlan {
    plan: OrderBookApplyPlan,
}

#[derive(Debug, Clone)]
pub struct OrderBookEngineBuffers {
    pub allocation_count_proxy: usize,
    pub bytes_allocated_proxy: usize,
    pub cache_shape: &'static str,
}

#[derive(Debug)]
pub struct PreparedOrderBookEngine {
    engine: OrderBookEngine,
    buffers: OrderBookEngineBuffers,
    replay_count: usize,
}

#[derive(Debug)]
pub struct OrderBookReplaySession {
    plan: PreparedOrderBookApplyPlan,
    engine: PreparedOrderBookEngine,
}

#[derive(Debug)]
enum EngineStorage {
    Current(CurrentBook),
    Packed(PackedBook),
    Dense(DenseBook),
    Direct(DirectBook),
    RunLocality(RunLocalityBook),
    Paged(PagedBook),
    BTree(BTreeBook),
}

impl EngineStorage {
    fn reset(&mut self) {
        match self {
            Self::Current(book) => book.reset(),
            Self::Packed(book) => book.reset(),
            Self::Dense(book) => book.reset(),
            Self::Direct(book) => book.reset(),
            Self::RunLocality(book) => book.reset(),
            Self::Paged(book) => book.reset(),
            Self::BTree(book) => book.reset(),
        }
    }
}

#[derive(Debug, Default)]
struct MutableStats {
    adds: usize,
    modifies: usize,
    deletes: usize,
    zero_size_removes: usize,
    active_levels: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct BookKey {
    instrument: i64,
    side: BookSide,
    price: i64,
}

impl Default for BookKey {
    fn default() -> Self {
        Self {
            instrument: 0,
            side: BookSide::Other(0),
            price: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BookMutation {
    instrument: i64,
    side: BookSide,
    price: i64,
    size: i64,
    operation: OrderBookOperation,
    zero_size_remove: bool,
}

impl BookMutation {
    fn new(instrument: i64, side: BookSide, price: i64, size: i64, action: i64) -> Self {
        let operation = if size <= 0 || action == 2 {
            OrderBookOperation::Remove
        } else {
            OrderBookOperation::Set
        };
        Self {
            instrument,
            side,
            price,
            size,
            operation,
            zero_size_remove: size <= 0,
        }
    }

    fn from_delta(delta: OrderBookDelta) -> Self {
        Self {
            instrument: delta.instrument,
            side: delta.side,
            price: delta.price,
            size: delta.size,
            operation: delta.operation,
            zero_size_remove: delta.is_zero_size_remove(),
        }
    }

    const fn is_remove(self) -> bool {
        matches!(self.operation, OrderBookOperation::Remove)
    }
}

impl OrderBookApplyPlan {
    pub fn compile(deltas: &[OrderBookDelta], kind: OrderBookEngineKind) -> Result<Self> {
        if let OrderBookEngineKind::PagedLadder { page_size } = kind {
            if page_size == 0 || !page_size.is_power_of_two() {
                return Err(AuraError::InvalidValue("orderbook page size"));
            }
        }

        let mut instruments = Vec::with_capacity(deltas.len().min(4096));
        let mut prices = Vec::with_capacity(deltas.len());
        let mut ranges =
            HashMap::<(i64, BookSide), (i64, i64)>::with_capacity(deltas.len().min(4096));
        for delta in deltas {
            instruments.push(delta.instrument);
            prices.push(delta.price);
            ranges
                .entry((delta.instrument, delta.side))
                .and_modify(|range| {
                    range.0 = range.0.min(delta.price);
                    range.1 = range.1.max(delta.price);
                })
                .or_insert((delta.price, delta.price));
        }

        instruments.sort_unstable();
        instruments.dedup();
        prices.sort_unstable();
        prices.dedup();
        let instrument_min = instruments.first().copied().unwrap_or(0);
        let instrument_max = instruments.last().copied().unwrap_or(instrument_min);
        let instrument_span = instrument_max
            .checked_sub(instrument_min)
            .and_then(|span| span.checked_add(1))
            .and_then(|span| usize::try_from(span).ok())
            .ok_or(AuraError::InvalidValue("instrument range"))?;
        let mut instrument_lookup = vec![MISSING_INDEX; instrument_span.max(1)];
        for (index, instrument) in instruments.iter().copied().enumerate() {
            let offset = usize::try_from(instrument - instrument_min)
                .map_err(|_| AuraError::InvalidValue("instrument range"))?;
            instrument_lookup[offset] = index;
        }

        let mut side_lookup = vec![MISSING_INDEX; instruments.len().saturating_mul(SIDE_COUNT)];
        let mut side_plans = Vec::with_capacity(ranges.len());
        let mut total_slots = 0usize;
        let mut ranges = ranges.into_iter().collect::<Vec<_>>();
        ranges.sort_unstable_by_key(|((instrument, side), _)| (*instrument, side.raw()));
        for ((instrument, side), (min_price, max_price)) in ranges {
            let instrument_index =
                instrument_lookup_index(instrument, instrument_min, &instrument_lookup)
                    .ok_or(AuraError::InvalidValue("instrument index"))?;
            let side_index = side_plans.len();
            let len = usize::try_from(max_price - min_price + 1)
                .map_err(|_| AuraError::InvalidValue("price range"))?;
            side_lookup[instrument_index * SIDE_COUNT + usize::from(side.raw())] = side_index;
            side_plans.push(SidePlan {
                instrument,
                side,
                min_price,
                max_price,
                base_offset: total_slots,
            });
            total_slots = total_slots
                .checked_add(len)
                .ok_or(AuraError::InvalidValue("price range"))?;
        }

        Ok(Self {
            kind,
            record_count: deltas.len(),
            instruments,
            instrument_min,
            instrument_lookup,
            side_lookup,
            side_plans,
            price_levels: prices.len(),
            total_slots,
        })
    }

    pub fn instrument_count(&self) -> usize {
        self.instruments.len()
    }

    pub fn price_level_count(&self) -> usize {
        self.price_levels
    }

    pub fn total_slots(&self) -> usize {
        self.total_slots
    }

    fn side_index_raw(&self, instrument: i64, side: BookSide) -> Result<usize> {
        let instrument_index =
            instrument_lookup_index(instrument, self.instrument_min, &self.instrument_lookup)
                .ok_or(AuraError::InvalidValue("instrument index"))?;
        let side_index = self.side_lookup[instrument_index * SIDE_COUNT + usize::from(side.raw())];
        if side_index == MISSING_INDEX {
            return Err(AuraError::InvalidValue("book side index"));
        }
        Ok(side_index)
    }
}

impl PreparedOrderBookApplyPlan {
    pub fn build(deltas: &[OrderBookDelta], kind: OrderBookEngineKind) -> Result<Self> {
        Ok(Self {
            plan: OrderBookApplyPlan::compile(deltas, kind)?,
        })
    }

    pub const fn plan(&self) -> &OrderBookApplyPlan {
        &self.plan
    }
}

impl PreparedOrderBookEngine {
    pub fn with_capacity(plan: &PreparedOrderBookApplyPlan) -> Result<Self> {
        let engine = OrderBookEngine::with_plan(plan.plan.clone())?;
        let buffers = engine.buffers();
        Ok(Self {
            engine,
            buffers,
            replay_count: 0,
        })
    }

    pub fn reset_for_replay(&mut self) {
        self.engine.reset_for_replay();
    }

    pub fn apply_values(
        &mut self,
        instrument: i64,
        side: i64,
        price: i64,
        size: i64,
        action: i64,
    ) -> Result<()> {
        self.engine
            .apply_values(instrument, side, price, size, action)
    }

    pub fn finish_profiled(
        &mut self,
        mode: OrderBookApplyMode,
        expected_hash: Option<BookStateHash>,
        update_remove_ns: u128,
        reset_ns: u128,
    ) -> Result<BookApplyStats> {
        let engine_reused = true;
        let buffers_reused = true;
        let mut stats = self.engine.finish_profiled(
            mode,
            OrderBookLifecycleMode::Prepared,
            expected_hash,
            update_remove_ns,
            engine_reused,
            buffers_reused,
        )?;
        stats.allocation_count_proxy = self.buffers.allocation_count_proxy;
        stats.bytes_allocated_proxy = self.buffers.bytes_allocated_proxy;
        stats.cache_shape = self.buffers.cache_shape;
        stats.breakdown.reset_ns = reset_ns;
        self.replay_count = self.replay_count.saturating_add(1);
        Ok(stats)
    }

    pub fn apply(
        &mut self,
        deltas: &[OrderBookDelta],
        mode: OrderBookApplyMode,
        expected_hash: Option<BookStateHash>,
    ) -> Result<BookApplyStats> {
        let reset_start = Instant::now();
        self.reset_for_replay();
        let reset_ns = reset_start.elapsed().as_nanos();
        let engine_reused = true;
        let buffers_reused = true;
        let mut stats = match mode {
            OrderBookApplyMode::Production => self.engine.apply_all_production_profiled(
                deltas,
                OrderBookLifecycleMode::Prepared,
                engine_reused,
                buffers_reused,
            )?,
            OrderBookApplyMode::Verify => self.engine.apply_all_verify_profiled(
                deltas,
                OrderBookLifecycleMode::Prepared,
                engine_reused,
                buffers_reused,
                expected_hash,
            )?,
        };
        stats.allocation_count_proxy = self.buffers.allocation_count_proxy;
        stats.bytes_allocated_proxy = self.buffers.bytes_allocated_proxy;
        stats.cache_shape = self.buffers.cache_shape;
        stats.breakdown.reset_ns = reset_ns;
        self.replay_count = self.replay_count.saturating_add(1);
        Ok(stats)
    }
}

impl OrderBookReplaySession {
    pub fn prepare(deltas: &[OrderBookDelta], kind: OrderBookEngineKind) -> Result<Self> {
        let plan = PreparedOrderBookApplyPlan::build(deltas, kind)?;
        let engine = PreparedOrderBookEngine::with_capacity(&plan)?;
        Ok(Self { plan, engine })
    }

    pub fn from_prepared(
        plan: PreparedOrderBookApplyPlan,
        engine: PreparedOrderBookEngine,
    ) -> Self {
        Self { plan, engine }
    }

    pub const fn plan(&self) -> &PreparedOrderBookApplyPlan {
        &self.plan
    }

    pub fn engine_mut(&mut self) -> &mut PreparedOrderBookEngine {
        &mut self.engine
    }

    pub fn replay(
        &mut self,
        deltas: &[OrderBookDelta],
        mode: OrderBookApplyMode,
        expected_hash: Option<BookStateHash>,
    ) -> Result<BookApplyStats> {
        self.engine.apply(deltas, mode, expected_hash)
    }
}

impl OrderBookEngine {
    pub fn with_plan(plan: OrderBookApplyPlan) -> Result<Self> {
        let storage = match plan.kind {
            OrderBookEngineKind::Current => EngineStorage::Current(CurrentBook::with_plan(&plan)),
            OrderBookEngineKind::PackedKey => EngineStorage::Packed(PackedBook::with_plan(&plan)),
            OrderBookEngineKind::DenseLadder => EngineStorage::Dense(DenseBook::with_plan(&plan)),
            OrderBookEngineKind::PagedLadder { page_size } => {
                EngineStorage::Paged(PagedBook::with_plan(&plan, page_size))
            }
            OrderBookEngineKind::DirectIndex => EngineStorage::Direct(DirectBook::with_plan(&plan)),
            OrderBookEngineKind::RunLocality => {
                EngineStorage::RunLocality(RunLocalityBook::with_plan(&plan))
            }
            OrderBookEngineKind::Optimized => {
                let dense_limit = plan.record_count.saturating_mul(64).max(1_000_000);
                if (plan.instruments.len() <= 8 || plan.price_levels >= 8_192)
                    && plan.total_slots <= dense_limit
                {
                    EngineStorage::Dense(DenseBook::with_plan(&plan))
                } else if plan.total_slots <= dense_limit {
                    EngineStorage::Direct(DirectBook::with_plan(&plan))
                } else {
                    EngineStorage::Paged(PagedBook::with_plan(&plan, 64))
                }
            }
            OrderBookEngineKind::BTreeMap => EngineStorage::BTree(BTreeBook::with_plan(&plan)),
        };
        Ok(Self {
            plan,
            storage,
            stats: MutableStats::default(),
        })
    }

    pub fn apply(&mut self, delta: OrderBookDelta) -> Result<()> {
        self.apply_mutation(BookMutation::from_delta(delta))
    }

    pub fn apply_values(
        &mut self,
        instrument: i64,
        side: i64,
        price: i64,
        size: i64,
        action: i64,
    ) -> Result<()> {
        let side = BookSide::try_from_i64(side)?;
        self.apply_mutation(BookMutation::new(instrument, side, price, size, action))
    }

    fn apply_mutation(&mut self, mutation: BookMutation) -> Result<()> {
        let transition = match &mut self.storage {
            EngineStorage::Current(book) => book.apply(mutation),
            EngineStorage::Packed(book) => book.apply(mutation),
            EngineStorage::Dense(book) => book.apply(&self.plan, mutation),
            EngineStorage::Direct(book) => book.apply(&self.plan, mutation),
            EngineStorage::RunLocality(book) => book.apply(&self.plan, mutation),
            EngineStorage::Paged(book) => book.apply(&self.plan, mutation),
            EngineStorage::BTree(book) => book.apply(mutation),
        }?;
        self.stats.observe(mutation, transition);
        Ok(())
    }

    pub fn apply_all(&mut self, deltas: &[OrderBookDelta]) -> Result<BookApplyStats> {
        for &delta in deltas {
            self.apply(delta)?;
        }
        Ok(self.stats())
    }

    pub fn apply_all_profiled(&mut self, deltas: &[OrderBookDelta]) -> Result<BookApplyStats> {
        let loop_start = Instant::now();
        for &delta in deltas {
            self.apply(delta)?;
        }
        let update_remove_ns = loop_start.elapsed().as_nanos();

        let hash_start = Instant::now();
        let levels = self.active_levels();
        let state_hash = hash_book_levels(levels.iter().copied());
        let state_hash_ns = hash_start.elapsed().as_nanos();

        let breakdown = BookApplyBreakdown {
            update_remove_ns,
            state_hash_ns,
            ..BookApplyBreakdown::default()
        };
        Ok(self.stats_from_parts(
            levels.len(),
            state_hash,
            breakdown,
            OrderBookApplyMode::Verify,
            OrderBookLifecycleMode::Cold,
            true,
            false,
            false,
        ))
    }

    pub fn apply_all_production_profiled(
        &mut self,
        deltas: &[OrderBookDelta],
        lifecycle_mode: OrderBookLifecycleMode,
        engine_reused: bool,
        buffers_reused: bool,
    ) -> Result<BookApplyStats> {
        let loop_start = Instant::now();
        for &delta in deltas {
            self.apply(delta)?;
        }
        let update_remove_ns = loop_start.elapsed().as_nanos();
        let breakdown = BookApplyBreakdown {
            update_remove_ns,
            ..BookApplyBreakdown::default()
        };
        Ok(self.stats_from_parts(
            self.stats.active_levels,
            BookStateHash(0),
            breakdown,
            OrderBookApplyMode::Production,
            lifecycle_mode,
            false,
            engine_reused,
            buffers_reused,
        ))
    }

    pub fn apply_all_verify_profiled(
        &mut self,
        deltas: &[OrderBookDelta],
        lifecycle_mode: OrderBookLifecycleMode,
        engine_reused: bool,
        buffers_reused: bool,
        expected_hash: Option<BookStateHash>,
    ) -> Result<BookApplyStats> {
        let loop_start = Instant::now();
        for &delta in deltas {
            self.apply(delta)?;
        }
        let update_remove_ns = loop_start.elapsed().as_nanos();

        let hash_start = Instant::now();
        let levels = self.active_levels();
        let state_hash = hash_book_levels(levels.iter().copied());
        let state_hash_ns = hash_start.elapsed().as_nanos();
        if let Some(expected_hash) = expected_hash {
            if expected_hash != state_hash {
                return Err(AuraError::InvalidValue("orderbook state hash"));
            }
        }

        let breakdown = BookApplyBreakdown {
            update_remove_ns,
            state_hash_ns,
            ..BookApplyBreakdown::default()
        };
        Ok(self.stats_from_parts(
            levels.len(),
            state_hash,
            breakdown,
            OrderBookApplyMode::Verify,
            lifecycle_mode,
            expected_hash.is_some(),
            engine_reused,
            buffers_reused,
        ))
    }

    pub fn finish_profiled(
        &self,
        mode: OrderBookApplyMode,
        lifecycle_mode: OrderBookLifecycleMode,
        expected_hash: Option<BookStateHash>,
        update_remove_ns: u128,
        engine_reused: bool,
        buffers_reused: bool,
    ) -> Result<BookApplyStats> {
        match mode {
            OrderBookApplyMode::Production => {
                let breakdown = BookApplyBreakdown {
                    update_remove_ns,
                    ..BookApplyBreakdown::default()
                };
                Ok(self.stats_from_parts(
                    self.stats.active_levels,
                    BookStateHash(0),
                    breakdown,
                    mode,
                    lifecycle_mode,
                    false,
                    engine_reused,
                    buffers_reused,
                ))
            }
            OrderBookApplyMode::Verify => {
                let hash_start = Instant::now();
                let levels = self.active_levels();
                let state_hash = hash_book_levels(levels.iter().copied());
                let state_hash_ns = hash_start.elapsed().as_nanos();
                if let Some(expected_hash) = expected_hash {
                    if expected_hash != state_hash {
                        return Err(AuraError::InvalidValue("orderbook state hash"));
                    }
                }
                let breakdown = BookApplyBreakdown {
                    update_remove_ns,
                    state_hash_ns,
                    ..BookApplyBreakdown::default()
                };
                Ok(self.stats_from_parts(
                    levels.len(),
                    state_hash,
                    breakdown,
                    mode,
                    lifecycle_mode,
                    expected_hash.is_some(),
                    engine_reused,
                    buffers_reused,
                ))
            }
        }
    }

    pub fn reset_for_replay(&mut self) {
        self.storage.reset();
        self.stats = MutableStats::default();
    }

    pub fn stats(&self) -> BookApplyStats {
        let levels = self.active_levels();
        let state_hash = hash_book_levels(levels.iter().copied());
        self.stats_from_parts(
            levels.len(),
            state_hash,
            BookApplyBreakdown::default(),
            OrderBookApplyMode::Verify,
            OrderBookLifecycleMode::Cold,
            true,
            false,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn stats_from_parts(
        &self,
        book_levels: usize,
        state_hash: BookStateHash,
        breakdown: BookApplyBreakdown,
        apply_mode: OrderBookApplyMode,
        lifecycle_mode: OrderBookLifecycleMode,
        state_hash_checked: bool,
        engine_reused: bool,
        buffers_reused: bool,
    ) -> BookApplyStats {
        let buffers = self.buffers();
        BookApplyStats {
            apply_mode,
            lifecycle_mode,
            engine_kind: self.plan.kind.as_str(),
            book_levels,
            instruments: self.plan.instrument_count(),
            price_levels: self.plan.price_level_count(),
            adds: self.stats.adds,
            modifies: self.stats.modifies,
            deletes: self.stats.deletes,
            zero_size_removes: self.stats.zero_size_removes,
            state_hash,
            allocation_count_proxy: buffers.allocation_count_proxy,
            bytes_allocated_proxy: buffers.bytes_allocated_proxy,
            cache_shape: buffers.cache_shape,
            breakdown,
            state_hash_checked,
            engine_reused,
            buffers_reused,
        }
    }

    pub fn buffers(&self) -> OrderBookEngineBuffers {
        let (allocation_count_proxy, bytes_allocated_proxy, cache_shape) = match &self.storage {
            EngineStorage::Current(book) => (
                book.slots.len(),
                book.slots.len() * std::mem::size_of::<CurrentSlot>(),
                "open-addressed-current",
            ),
            EngineStorage::Packed(book) => (
                book.levels.capacity(),
                book.levels.capacity() * std::mem::size_of::<(u128, i64)>(),
                "packed-key-hash-map",
            ),
            EngineStorage::Dense(book) => (
                book.levels.len(),
                book.levels.iter().map(Vec::capacity).sum::<usize>() * std::mem::size_of::<i64>(),
                "dense-instrument-side-ladder",
            ),
            EngineStorage::Direct(book) => (
                1,
                book.levels.len() * std::mem::size_of::<i64>(),
                "flat-direct-index",
            ),
            EngineStorage::RunLocality(book) => (
                1,
                book.direct.levels.len() * std::mem::size_of::<i64>(),
                "flat-direct-index-run-cache",
            ),
            EngineStorage::Paged(book) => (
                book.allocated_pages,
                book.allocated_pages * book.page_size * std::mem::size_of::<i64>(),
                "paged-ladder",
            ),
            EngineStorage::BTree(book) => (
                book.levels.len(),
                book.levels.len() * std::mem::size_of::<(BookKey, i64)>(),
                "btree-map",
            ),
        };
        OrderBookEngineBuffers {
            allocation_count_proxy,
            bytes_allocated_proxy,
            cache_shape,
        }
    }

    pub fn active_levels(&self) -> Vec<BookLevel> {
        match &self.storage {
            EngineStorage::Current(book) => book.active_levels(),
            EngineStorage::Packed(book) => book.active_levels(),
            EngineStorage::Dense(book) => book.active_levels(&self.plan),
            EngineStorage::Direct(book) => book.active_levels(&self.plan),
            EngineStorage::RunLocality(book) => book.direct.active_levels(&self.plan),
            EngineStorage::Paged(book) => book.active_levels(&self.plan),
            EngineStorage::BTree(book) => book.active_levels(),
        }
    }
}

impl MutableStats {
    fn observe(&mut self, mutation: BookMutation, transition: BookTransition) {
        if mutation.zero_size_remove {
            self.zero_size_removes = self.zero_size_removes.saturating_add(1);
        }
        match transition {
            BookTransition::Add => {
                self.adds = self.adds.saturating_add(1);
                self.active_levels = self.active_levels.saturating_add(1);
            }
            BookTransition::Modify => self.modifies = self.modifies.saturating_add(1),
            BookTransition::Delete => {
                self.deletes = self.deletes.saturating_add(1);
                self.active_levels = self.active_levels.saturating_sub(1);
            }
            BookTransition::RemoveMissing => self.deletes = self.deletes.saturating_add(1),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum BookTransition {
    Add,
    Modify,
    Delete,
    RemoveMissing,
}

#[derive(Debug)]
struct CurrentBook {
    slots: Vec<CurrentSlot>,
    mask: usize,
    level_count: usize,
}

impl CurrentBook {
    fn with_plan(plan: &OrderBookApplyPlan) -> Self {
        let capacity = plan.record_count.saturating_mul(2);
        let table_len = capacity.min(16 * 1024).next_power_of_two().max(1024);
        Self {
            slots: vec![CurrentSlot::default(); table_len],
            mask: table_len - 1,
            level_count: 0,
        }
    }

    fn apply(&mut self, mutation: BookMutation) -> Result<BookTransition> {
        let key = BookKey {
            instrument: mutation.instrument,
            side: mutation.side,
            price: mutation.price,
        };
        if mutation.is_remove() {
            Ok(if self.delete(key) {
                BookTransition::Delete
            } else {
                BookTransition::RemoveMissing
            })
        } else {
            Ok(self.upsert(key, mutation.size))
        }
    }

    fn active_levels(&self) -> Vec<BookLevel> {
        self.slots
            .iter()
            .filter(|slot| slot.state == 1)
            .map(|slot| BookLevel {
                instrument: slot.key.instrument,
                side: slot.key.side,
                price: slot.key.price,
                size: slot.size,
            })
            .collect()
    }

    fn reset(&mut self) {
        for slot in &mut self.slots {
            *slot = CurrentSlot::default();
        }
        self.level_count = 0;
    }

    fn upsert(&mut self, key: BookKey, size: i64) -> BookTransition {
        if self.level_count.saturating_mul(10) >= self.slots.len().saturating_mul(7) {
            self.grow();
        }
        let (index, found) = self.find_slot(key);
        let slot = &mut self.slots[index];
        if found {
            slot.size = size;
            BookTransition::Modify
        } else {
            slot.key = key;
            slot.size = size;
            slot.state = 1;
            self.level_count = self.level_count.saturating_add(1);
            BookTransition::Add
        }
    }

    fn delete(&mut self, key: BookKey) -> bool {
        let (index, found) = self.find_slot(key);
        if found {
            let slot = &mut self.slots[index];
            slot.size = 0;
            slot.state = 2;
            self.level_count = self.level_count.saturating_sub(1);
            true
        } else {
            false
        }
    }

    fn grow(&mut self) {
        let old_slots = std::mem::take(&mut self.slots);
        let table_len = (old_slots.len().saturating_mul(2)).max(1024);
        self.slots = vec![CurrentSlot::default(); table_len];
        self.mask = table_len - 1;
        self.level_count = 0;
        for slot in old_slots {
            if slot.state == 1 {
                let _ = self.upsert(slot.key, slot.size);
            }
        }
    }

    fn find_slot(&self, key: BookKey) -> (usize, bool) {
        let mut index = hash_book_key(key) as usize & self.mask;
        let mut first_tombstone = None;
        loop {
            let slot = self.slots[index];
            match slot.state {
                0 => return (first_tombstone.unwrap_or(index), false),
                1 if slot.key == key => return (index, true),
                2 if first_tombstone.is_none() => first_tombstone = Some(index),
                _ => {}
            }
            index = (index + 1) & self.mask;
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CurrentSlot {
    key: BookKey,
    size: i64,
    state: u8,
}

#[derive(Debug)]
struct PackedBook {
    levels: HashMap<u128, i64>,
}

impl PackedBook {
    fn with_plan(plan: &OrderBookApplyPlan) -> Self {
        Self {
            levels: HashMap::with_capacity(plan.record_count.saturating_mul(2)),
        }
    }

    fn apply(&mut self, mutation: BookMutation) -> Result<BookTransition> {
        let packed = pack_book_key(BookKey {
            instrument: mutation.instrument,
            side: mutation.side,
            price: mutation.price,
        })?;
        if mutation.is_remove() {
            Ok(if self.levels.remove(&packed).is_some() {
                BookTransition::Delete
            } else {
                BookTransition::RemoveMissing
            })
        } else if let std::collections::hash_map::Entry::Occupied(mut entry) =
            self.levels.entry(packed)
        {
            entry.insert(mutation.size);
            Ok(BookTransition::Modify)
        } else {
            self.levels.insert(packed, mutation.size);
            Ok(BookTransition::Add)
        }
    }

    fn active_levels(&self) -> Vec<BookLevel> {
        self.levels
            .iter()
            .filter_map(|(packed, size)| {
                unpack_book_key(*packed).ok().map(|key| BookLevel {
                    instrument: key.instrument,
                    side: key.side,
                    price: key.price,
                    size: *size,
                })
            })
            .collect()
    }

    fn reset(&mut self) {
        self.levels.clear();
    }
}

#[derive(Debug)]
struct BTreeBook {
    levels: BTreeMap<BookKey, i64>,
}

impl BTreeBook {
    fn with_plan(_plan: &OrderBookApplyPlan) -> Self {
        Self {
            levels: BTreeMap::new(),
        }
    }

    fn apply(&mut self, mutation: BookMutation) -> Result<BookTransition> {
        let key = BookKey {
            instrument: mutation.instrument,
            side: mutation.side,
            price: mutation.price,
        };
        if mutation.is_remove() {
            Ok(if self.levels.remove(&key).is_some() {
                BookTransition::Delete
            } else {
                BookTransition::RemoveMissing
            })
        } else if let std::collections::btree_map::Entry::Occupied(mut entry) =
            self.levels.entry(key)
        {
            entry.insert(mutation.size);
            Ok(BookTransition::Modify)
        } else {
            self.levels.insert(key, mutation.size);
            Ok(BookTransition::Add)
        }
    }

    fn active_levels(&self) -> Vec<BookLevel> {
        self.levels
            .iter()
            .map(|(key, size)| BookLevel {
                instrument: key.instrument,
                side: key.side,
                price: key.price,
                size: *size,
            })
            .collect()
    }

    fn reset(&mut self) {
        self.levels.clear();
    }
}

#[derive(Debug)]
struct DenseBook {
    levels: Vec<Vec<i64>>,
}

impl DenseBook {
    fn with_plan(plan: &OrderBookApplyPlan) -> Self {
        Self {
            levels: plan
                .side_plans
                .iter()
                .map(|side| vec![0; side.len()])
                .collect(),
        }
    }

    fn apply(
        &mut self,
        plan: &OrderBookApplyPlan,
        mutation: BookMutation,
    ) -> Result<BookTransition> {
        let side_index = plan.side_index_raw(mutation.instrument, mutation.side)?;
        let side = plan.side_plans[side_index];
        let offset = price_offset(side, mutation.price)?;
        let slot = self
            .levels
            .get_mut(side_index)
            .and_then(|levels| levels.get_mut(offset))
            .ok_or(AuraError::UnexpectedEof)?;
        Ok(apply_slot(slot, mutation))
    }

    fn active_levels(&self, plan: &OrderBookApplyPlan) -> Vec<BookLevel> {
        let mut out = Vec::new();
        for (side_index, levels) in self.levels.iter().enumerate() {
            let side = plan.side_plans[side_index];
            for (offset, size) in levels.iter().enumerate().filter(|(_, size)| **size != 0) {
                out.push(BookLevel {
                    instrument: side.instrument,
                    side: side.side,
                    price: side.min_price + offset as i64,
                    size: *size,
                });
            }
        }
        out
    }

    fn reset(&mut self) {
        for levels in &mut self.levels {
            levels.fill(0);
        }
    }
}

#[derive(Debug)]
struct DirectBook {
    levels: Vec<i64>,
}

impl DirectBook {
    fn with_plan(plan: &OrderBookApplyPlan) -> Self {
        Self {
            levels: vec![0; plan.total_slots],
        }
    }

    fn apply(
        &mut self,
        plan: &OrderBookApplyPlan,
        mutation: BookMutation,
    ) -> Result<BookTransition> {
        let index = direct_level_index(plan, mutation)?;
        let slot = self.levels.get_mut(index).ok_or(AuraError::UnexpectedEof)?;
        Ok(apply_slot(slot, mutation))
    }

    fn active_levels(&self, plan: &OrderBookApplyPlan) -> Vec<BookLevel> {
        let mut out = Vec::new();
        for side in &plan.side_plans {
            for offset in 0..side.len() {
                let index = side.base_offset + offset;
                let size = self.levels[index];
                if size != 0 {
                    out.push(BookLevel {
                        instrument: side.instrument,
                        side: side.side,
                        price: side.min_price + offset as i64,
                        size,
                    });
                }
            }
        }
        out
    }

    fn reset(&mut self) {
        self.levels.fill(0);
    }
}

#[derive(Debug)]
struct RunLocalityBook {
    direct: DirectBook,
    last_instrument: i64,
    last_side: BookSide,
    last_side_index: usize,
    has_last: bool,
}

impl RunLocalityBook {
    fn with_plan(plan: &OrderBookApplyPlan) -> Self {
        Self {
            direct: DirectBook::with_plan(plan),
            last_instrument: 0,
            last_side: BookSide::Other(0),
            last_side_index: 0,
            has_last: false,
        }
    }

    fn apply(
        &mut self,
        plan: &OrderBookApplyPlan,
        mutation: BookMutation,
    ) -> Result<BookTransition> {
        let side_index = if self.has_last
            && self.last_instrument == mutation.instrument
            && self.last_side == mutation.side
        {
            self.last_side_index
        } else {
            let side_index = plan.side_index_raw(mutation.instrument, mutation.side)?;
            self.last_instrument = mutation.instrument;
            self.last_side = mutation.side;
            self.last_side_index = side_index;
            self.has_last = true;
            side_index
        };
        let side = plan.side_plans[side_index];
        let offset = price_offset(side, mutation.price)?;
        let index = side.base_offset + offset;
        let slot = self
            .direct
            .levels
            .get_mut(index)
            .ok_or(AuraError::UnexpectedEof)?;
        Ok(apply_slot(slot, mutation))
    }

    fn reset(&mut self) {
        self.direct.reset();
        self.has_last = false;
    }
}

#[derive(Debug)]
struct PagedBook {
    sides: Vec<PagedSide>,
    page_size: usize,
    allocated_pages: usize,
}

#[derive(Debug)]
struct PagedSide {
    min_price: i64,
    pages: Vec<Option<Box<[i64]>>>,
}

impl PagedBook {
    fn with_plan(plan: &OrderBookApplyPlan, page_size: usize) -> Self {
        let sides = plan
            .side_plans
            .iter()
            .map(|side| {
                let page_count = side.len().div_ceil(page_size);
                PagedSide {
                    min_price: side.min_price,
                    pages: vec![None; page_count],
                }
            })
            .collect();
        Self {
            sides,
            page_size,
            allocated_pages: 0,
        }
    }

    fn apply(
        &mut self,
        plan: &OrderBookApplyPlan,
        mutation: BookMutation,
    ) -> Result<BookTransition> {
        let side_index = plan.side_index_raw(mutation.instrument, mutation.side)?;
        let side_plan = plan.side_plans[side_index];
        let offset = price_offset(side_plan, mutation.price)?;
        let page_index = offset / self.page_size;
        let page_offset = offset % self.page_size;
        let side = self
            .sides
            .get_mut(side_index)
            .ok_or(AuraError::UnexpectedEof)?;
        if side.pages[page_index].is_none() {
            if mutation.is_remove() {
                return Ok(BookTransition::RemoveMissing);
            }
            side.pages[page_index] = Some(vec![0; self.page_size].into_boxed_slice());
            self.allocated_pages = self.allocated_pages.saturating_add(1);
        }
        let slot = side.pages[page_index]
            .as_mut()
            .and_then(|page| page.get_mut(page_offset))
            .ok_or(AuraError::UnexpectedEof)?;
        Ok(apply_slot(slot, mutation))
    }

    fn active_levels(&self, plan: &OrderBookApplyPlan) -> Vec<BookLevel> {
        let mut out = Vec::new();
        for (side_index, side) in self.sides.iter().enumerate() {
            let side_plan = plan.side_plans[side_index];
            for (page_index, page) in side.pages.iter().enumerate() {
                let Some(page) = page else {
                    continue;
                };
                for (page_offset, size) in page.iter().enumerate().filter(|(_, size)| **size != 0) {
                    let offset = page_index * self.page_size + page_offset;
                    if offset >= side_plan.len() {
                        continue;
                    }
                    out.push(BookLevel {
                        instrument: side_plan.instrument,
                        side: side_plan.side,
                        price: side.min_price + offset as i64,
                        size: *size,
                    });
                }
            }
        }
        out
    }

    fn reset(&mut self) {
        for side in &mut self.sides {
            for page in side.pages.iter_mut().flatten() {
                page.fill(0);
            }
        }
    }
}

fn apply_slot(slot: &mut i64, mutation: BookMutation) -> BookTransition {
    if mutation.is_remove() {
        if *slot == 0 {
            return BookTransition::RemoveMissing;
        }
        *slot = 0;
        BookTransition::Delete
    } else if *slot == 0 {
        *slot = mutation.size;
        BookTransition::Add
    } else {
        *slot = mutation.size;
        BookTransition::Modify
    }
}

fn direct_level_index(plan: &OrderBookApplyPlan, mutation: BookMutation) -> Result<usize> {
    let side_index = plan.side_index_raw(mutation.instrument, mutation.side)?;
    let side = plan.side_plans[side_index];
    Ok(side.base_offset + price_offset(side, mutation.price)?)
}

fn price_offset(side: SidePlan, price: i64) -> Result<usize> {
    if price < side.min_price || price > side.max_price {
        return Err(AuraError::InvalidValue("price range"));
    }
    usize::try_from(price - side.min_price).map_err(|_| AuraError::InvalidValue("price range"))
}

fn instrument_lookup_index(instrument: i64, min: i64, lookup: &[usize]) -> Option<usize> {
    let offset = usize::try_from(instrument.checked_sub(min)?).ok()?;
    let index = lookup.get(offset).copied()?;
    (index != MISSING_INDEX).then_some(index)
}

fn pack_book_key(key: BookKey) -> Result<u128> {
    if !fits_signed_bits(key.instrument, 48) {
        return Err(AuraError::InvalidValue("packed book instrument"));
    }
    let instrument = (i128::from(key.instrument) & ((1i128 << 48) - 1)) as u128;
    let side = u128::from(key.side.raw());
    let price = key.price as u64 as u128;
    Ok((instrument << 80) | (side << 64) | price)
}

fn unpack_book_key(packed: u128) -> Result<BookKey> {
    let instrument = sign_extend_i64(packed >> 80, 48)?;
    let side = BookSide::try_from_i64(((packed >> 64) & 0xff) as i64)?;
    let price = packed as u64 as i64;
    Ok(BookKey {
        instrument,
        side,
        price,
    })
}

fn fits_signed_bits(value: i64, bits: u32) -> bool {
    let min = -(1i128 << (bits - 1));
    let max = (1i128 << (bits - 1)) - 1;
    let value = i128::from(value);
    value >= min && value <= max
}

fn sign_extend_i64(value: u128, bits: u32) -> Result<i64> {
    let shift = 128 - bits;
    let signed = ((value << shift) as i128) >> shift;
    i64::try_from(signed).map_err(|_| AuraError::InvalidValue("packed book key"))
}

fn hash_book_key(key: BookKey) -> u64 {
    let mut hash = (key.instrument as u64).wrapping_mul(0x9E37_79B1_85EB_CA87);
    hash ^= u64::from(key.side.raw())
        .rotate_left(17)
        .wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    hash ^= (key.price as u64)
        .rotate_left(31)
        .wrapping_mul(0x1656_67B1_9E37_79F9);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    hash ^ (hash >> 29)
}

pub fn hash_book_levels(levels: impl IntoIterator<Item = BookLevel>) -> BookStateHash {
    let mut levels = levels.into_iter().collect::<Vec<_>>();
    levels.sort_unstable_by_key(|level| (level.instrument, level.side.raw(), level.price));
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for level in levels {
        hash = mix_checksum(hash, level.instrument);
        hash = mix_checksum(hash, level.side.raw_i64());
        hash = mix_checksum(hash, level.price);
        hash = mix_checksum(hash, level.size);
    }
    BookStateHash(hash.wrapping_add(0x9e37_79b9_7f4a_7c15))
}

fn mix_checksum(checksum: u64, value: i64) -> u64 {
    checksum.wrapping_mul(0x9E37_79B1_85EB_CA87).rotate_left(7)
        ^ (value as u64).wrapping_add(0xC2B2_AE3D_27D4_EB4F)
}
