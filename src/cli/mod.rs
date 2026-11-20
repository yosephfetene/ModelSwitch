//! Operator commands of the service binary.

pub mod bench;
pub mod keys;
pub mod pricing;

use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

const TIME_FORMAT: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");

/// A stored time as operators see it: UTC, to the second.
fn utc_to_the_second(time: OffsetDateTime) -> anyhow::Result<String> {
    Ok(time.to_offset(UtcOffset::UTC).format(TIME_FORMAT)?)
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn times_are_shown_in_utc_to_the_second() {
        let time = datetime!(2026-11-12 03:04:05.678 +03:00);

        assert_eq!(utc_to_the_second(time).unwrap(), "2026-11-12 00:04:05");
    }
}
