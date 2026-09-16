//! The family-projection contract.
//!
//! A provider turns raw directory reads into one typed report. It depends on
//! [`StatsReader`](crate::StatsReader) only, holds no connection or mapping,
//! caches no directory index, and keeps no baseline between reports.

use crate::StatsReader;
use crate::error::Error;

pub trait StatsProvider {
    /// The family directory prefix, such as `"/mem"` or `"/sys"`.
    const PREFIX: &'static str;
    /// The result of one complete read of that family.
    type Report;

    fn report<R: StatsReader>(reader: &R) -> Result<Self::Report, Error>;
}
