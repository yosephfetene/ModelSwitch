//! Validated values for versioned prices (ADR-034).
//!
//! Prices stay text until PostgreSQL's exact `numeric` type holds them, so
//! `0.10` never passes through floating point. Only plain decimals are
//! accepted: `numeric` would also take `NaN` and `1e5`.

const INTEGER_DIGITS_MAX: usize = 6;
const FRACTION_DIGITS_MAX: usize = 9;
const SOURCE_MAX_CHARS: usize = 2048;

/// A price in USD per million tokens, such as `0.10`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceUsd(String);

impl PriceUsd {
    pub fn parse(price: &str) -> Result<Self, PricingError> {
        let (integer, fraction) = match price.split_once('.') {
            Some((integer, fraction)) => (integer, Some(fraction)),
            None => (price, None),
        };
        let digits = |text: &str, max: usize| {
            (1..=max).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit())
        };
        if digits(integer, INTEGER_DIGITS_MAX)
            && fraction.is_none_or(|fraction| digits(fraction, FRACTION_DIGITS_MAX))
        {
            Ok(Self(price.to_owned()))
        } else {
            Err(PricingError::InvalidPrice)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where a price came from, such as the provider's pricing page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceSource(String);

impl PriceSource {
    pub fn parse(source: &str) -> Result<Self, PricingError> {
        let source = source.trim();
        let length = source.chars().count();
        if (1..=SOURCE_MAX_CHARS).contains(&length) && !source.chars().any(char::is_control) {
            Ok(Self(source.to_owned()))
        } else {
            Err(PricingError::InvalidSource)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PricingError {
    #[error(
        "price must be a plain decimal such as 0.10, with at most 6 digits before the point \
         and 9 after"
    )]
    InvalidPrice,
    #[error("price source must be 1-2048 characters with no control characters")]
    InvalidSource,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_decimals_are_prices() {
        for price in ["0", "3", "0.10", "999999.999999999"] {
            assert_eq!(PriceUsd::parse(price).unwrap().as_str(), price);
        }
    }

    #[test]
    fn anything_else_is_not_a_price() {
        for price in [
            "",
            ".",
            ".5",
            "5.",
            "-1",
            "+1",
            " 1",
            "1 ",
            "1e5",
            "NaN",
            "Infinity",
            "1,000",
            "0x10",
            "1.2.3",
            "٣",
            "1000000",
            "0.1234567890",
        ] {
            assert_eq!(
                PriceUsd::parse(price),
                Err(PricingError::InvalidPrice),
                "{price:?}"
            );
        }
    }

    #[test]
    fn sources_are_trimmed_and_validated() {
        let source = PriceSource::parse("  https://example.com/pricing  ").unwrap();
        assert_eq!(source.as_str(), "https://example.com/pricing");
        assert!(PriceSource::parse(&"x".repeat(2048)).is_ok());

        for source in ["", "   ", "evil\nINFO forged", &"x".repeat(2049)] {
            assert_eq!(
                PriceSource::parse(source),
                Err(PricingError::InvalidSource),
                "{source:?}"
            );
        }
    }
}
