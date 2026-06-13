use crate::{AuraError, Result};

pub const DEFAULT_DECIMAL_SCALE: i64 = 100_000_000;
pub const NANOS_PER_SECOND: i64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OhlcvF64 {
    pub ts_seconds: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecimalScales {
    pub price: i64,
    pub volume: i64,
}

impl Default for DecimalScales {
    fn default() -> Self {
        Self {
            price: DEFAULT_DECIMAL_SCALE,
            volume: DEFAULT_DECIMAL_SCALE,
        }
    }
}

pub fn ohlcv_i64_row(row: OhlcvF64, scales: DecimalScales) -> Result<Vec<i64>> {
    Ok(vec![
        seconds_to_ns(row.ts_seconds)?,
        scale_f64(row.open, scales.price)?,
        scale_f64(row.high, scales.price)?,
        scale_f64(row.low, scales.price)?,
        scale_f64(row.close, scales.price)?,
        scale_f64(row.volume, scales.volume)?,
    ])
}

pub fn seconds_to_ns(ts_seconds: i64) -> Result<i64> {
    ts_seconds
        .checked_mul(NANOS_PER_SECOND)
        .ok_or(AuraError::InvalidValue("timestamp seconds"))
}

pub fn scale_f64(value: f64, scale: i64) -> Result<i64> {
    if !value.is_finite() {
        return Err(AuraError::InvalidValue("finite float"));
    }
    if scale <= 0 {
        return Err(AuraError::InvalidValue("decimal scale"));
    }
    let scaled = value * scale as f64;
    if scaled < i64::MIN as f64 || scaled > i64::MAX as f64 {
        return Err(AuraError::InvalidValue("scaled float"));
    }
    Ok(scaled.round() as i64)
}
