//! Exact decimals: FIX prices and quantities without floats.
//!
//! FIX sends prices as decimal text (`44=187.25`). Parsing that into an
//! `f64` and multiplying by 100 gives `18724.999999999996` on a bad day, so
//! the order path never touches floats: the text is parsed into an integer
//! mantissa and a decimal scale (`18725 × 10⁻²`) and converted to integer
//! ticks with integer arithmetic, failing loudly if the price is not on a
//! tick.

/// Powers of ten that fit in an `i64`.
const POW10: [i64; 19] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    10_000_000_000_000,
    100_000_000_000_000,
    1_000_000_000_000_000,
    10_000_000_000_000_000,
    100_000_000_000_000_000,
    1_000_000_000_000_000_000,
];

/// `10^n` for `n <= 18`.
#[inline]
pub const fn pow10(n: u8) -> i64 {
    POW10[n as usize]
}

/// How many integer ticks make one unit of price: ticks = price × 10^decimals.
/// `PriceScale::new(2)` is cents: `187.25` ↔ `18725` ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PriceScale {
    decimals: u8,
}

impl PriceScale {
    /// Whole units (`187` ↔ `187`).
    pub const UNITS: PriceScale = PriceScale { decimals: 0 };
    /// Hundredths (`187.25` ↔ `18725`).
    pub const CENTS: PriceScale = PriceScale { decimals: 2 };

    /// A scale with `decimals` implied decimal places. Panics if above 18.
    pub const fn new(decimals: u8) -> PriceScale {
        assert!(decimals <= 18, "at most 18 decimals fit in an i64");
        PriceScale { decimals }
    }

    /// Implied decimal places.
    pub const fn decimals(self) -> u8 {
        self.decimals
    }
}

/// An exact decimal number: `mantissa × 10^-scale`.
///
/// Equality is structural (`1.50` and `1.5` differ); compare values with
/// [`Decimal::to_ticks`] at a common scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Decimal {
    /// All digits as one integer, sign included.
    pub mantissa: i64,
    /// Digits after the decimal point (0..=18).
    pub scale: u8,
}

// @student w14
impl Decimal {
    /// Parses FIX decimal text: optional `-`, digits, optional `.` and
    /// digits (`187.25`, `-0.5`, `.5`, `100.`, `007`). No exponent, no `+`.
    /// `None` if malformed, more than 18 decimals, or out of `i64` range.
    /// O(n), no allocation, no floats.
    pub fn parse(text: &[u8]) -> Option<Decimal> {
        let (negative, digits) = match text.split_first() {
            Some((b'-', rest)) => (true, rest),
            _ => (false, text),
        };
        let mut mantissa: i64 = 0;
        let mut scale: u8 = 0;
        let mut seen_point = false;
        let mut seen_digit = false;
        for &b in digits {
            match b {
                b'0'..=b'9' => {
                    seen_digit = true;
                    mantissa = mantissa.checked_mul(10)?.checked_add((b - b'0') as i64)?;
                    if seen_point {
                        scale += 1;
                        if scale > 18 {
                            return None;
                        }
                    }
                }
                b'.' if !seen_point => seen_point = true,
                _ => return None,
            }
        }
        if !seen_digit {
            return None;
        }
        Some(Decimal {
            mantissa: if negative { -mantissa } else { mantissa },
            scale,
        })
    }

    /// The decimal worth `ticks` at `scale` (exact). O(1).
    #[inline]
    pub fn from_ticks(ticks: i64, scale: PriceScale) -> Decimal {
        Decimal {
            mantissa: ticks,
            scale: scale.decimals(),
        }
    }

    /// Integer ticks at `scale`; `None` if the value is not a whole number
    /// of ticks (`187.255` at cents) or overflows. O(1), integer only.
    #[inline]
    pub fn to_ticks(self, scale: PriceScale) -> Option<i64> {
        let want = scale.decimals();
        if want >= self.scale {
            self.mantissa.checked_mul(pow10(want - self.scale))
        } else {
            let div = pow10(self.scale - want);
            (self.mantissa % div == 0).then(|| self.mantissa / div)
        }
    }

    /// Appends the canonical text: exactly `scale` digits after the point
    /// (`Decimal { mantissa: 5, scale: 2 }` → `0.05`). O(digits).
    pub fn write(self, out: &mut Vec<u8>) {
        let mut digits = [0u8; 20];
        let mut n = 0;
        let mut v = self.mantissa.unsigned_abs();
        while v > 0 || n <= self.scale as usize {
            digits[n] = b'0' + (v % 10) as u8;
            v /= 10;
            n += 1;
        }
        if self.mantissa < 0 {
            out.push(b'-');
        }
        for i in (0..n).rev() {
            out.push(digits[i]);
            if i == self.scale as usize && i > 0 {
                out.push(b'.');
            }
        }
    }
}
