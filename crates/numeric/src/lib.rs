//! Sums of numbers, exact before they are rounded, shared by the compiler
//! (which evaluates `sum` when compiling) and the runtime (which evaluates
//! it when running), so that the two can't disagree. A relation's tuples
//! have no order, and neither has a sum here: it is the exact sum of the
//! values, rounded once, which no order of adding them can change. It has
//! no dependencies and doesn't need `std`, so both can use it.

#![cfg_attr(not(test), no_std)]

/// The exact sum of integers. Each is below 2^63 in magnitude, and a
/// relation has fewer than 2^64 tuples, so the sum is below 2^127 in
/// magnitude, and an `i128` always holds it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IntegerSum(i128);

impl IntegerSum {
    pub fn add(&mut self, n: i64) {
        self.0 += i128::from(n);
    }

    /// The sum, or `None` if it is out of an integer's range.
    #[must_use]
    pub fn total(self) -> Option<i64> {
        i64::try_from(self.0).ok()
    }
}

/// The 64-bit limbs of [`ApproximateSum`]'s accumulator. Every finite
/// binary64 is a multiple of 2^-1074 below 2^1024 in magnitude, so 2098
/// bits hold one in units of 2^-1074; 64 more hold the carries of 2^64 of
/// them, and one more the sign: 2163 bits, in 34 limbs.
const LIMBS: usize = 34;

/// The exact sum of finite approximate numbers (IEEE 754 binary64), held as
/// an integer number of units of 2^-1074, the smallest subnormal, in two's
/// complement (a Kulisch accumulator), and rounded once, to nearest, ties
/// to even.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApproximateSum {
    /// Least significant first.
    limbs: [u64; LIMBS],
    /// Whether a value has been added.
    added: bool,
    /// Whether every value added is `-0.0`. As IEEE 754 addition in any
    /// order, a sum that is exactly zero is `-0.0` only then.
    negative_zero: bool,
}

impl Default for ApproximateSum {
    fn default() -> Self {
        ApproximateSum {
            limbs: [0; LIMBS],
            added: false,
            negative_zero: true,
        }
    }
}

const FRACTION_BITS: u32 = 52;
const IMPLICIT: u64 = 1 << FRACTION_BITS;

impl ApproximateSum {
    /// Add `x`, which must be finite: an infinity or a NaN has no exact
    /// value, and the total would mean nothing.
    pub fn add(&mut self, x: f64) {
        let bits = x.to_bits();
        self.added = true;
        self.negative_zero &= bits == (-0.0_f64).to_bits();
        // The sign, then 11 bits of biased exponent.
        let [sign_and_high, low, ..] = bits.to_be_bytes();
        let biased = usize::from(sign_and_high & 0x7F) << 4 | usize::from(low >> 4);
        let fraction = bits & (IMPLICIT - 1);
        // `x` is `significand` units of 2^-1074, shifted left by `shift`: a
        // subnormal's fraction is its count of units, and a normal number
        // has the implicit bit, at one exponent per shift.
        let (significand, shift) = match biased {
            0 => (fraction, 0),
            _ => (fraction | IMPLICIT, biased - 1),
        };
        if significand == 0 {
            return;
        }
        let (limb, offset) = (shift / 64, shift % 64);
        let low = significand << offset;
        let high = match offset {
            0 => 0,
            _ => significand >> (64 - offset),
        };
        if bits >> 63 == 0 {
            self.add_at(limb, low, high);
        } else {
            self.subtract_at(limb, low, high);
        }
    }

    /// Add `high` and `low`, the limbs `limb + 1` and `limb` of a number,
    /// carrying into the limbs above.
    fn add_at(&mut self, limb: usize, low: u64, high: u64) {
        let (sum, low_carry) = self.limbs[limb].overflowing_add(low);
        self.limbs[limb] = sum;
        let (sum, high_carry) = self.limbs[limb + 1].overflowing_add(high);
        let (sum, carry) = sum.overflowing_add(u64::from(low_carry));
        self.limbs[limb + 1] = sum;
        let mut carry = high_carry || carry;
        for limb in &mut self.limbs[limb + 2..] {
            if !carry {
                break;
            }
            (*limb, carry) = limb.overflowing_add(1);
        }
    }

    /// Subtract `high` and `low`, the limbs `limb + 1` and `limb` of a
    /// number, borrowing from the limbs above.
    fn subtract_at(&mut self, limb: usize, low: u64, high: u64) {
        let (difference, low_borrow) = self.limbs[limb].overflowing_sub(low);
        self.limbs[limb] = difference;
        let (difference, high_borrow) = self.limbs[limb + 1].overflowing_sub(high);
        let (difference, borrow) = difference.overflowing_sub(u64::from(low_borrow));
        self.limbs[limb + 1] = difference;
        let mut borrow = high_borrow || borrow;
        for limb in &mut self.limbs[limb + 2..] {
            if !borrow {
                break;
            }
            (*limb, borrow) = limb.overflowing_sub(1);
        }
    }

    /// The sum, rounded to nearest, ties to even, or `None` if that is
    /// beyond the largest finite approximate number. Of no values, it is
    /// `0.0`.
    #[must_use]
    pub fn total(&self) -> Option<f64> {
        let negative = self.limbs[LIMBS - 1] >> 63 == 1;
        let mut magnitude = self.limbs;
        if negative {
            // Two's complement: invert, and add one.
            let mut carry = true;
            for limb in &mut magnitude {
                (*limb, carry) = (!*limb).overflowing_add(u64::from(carry));
            }
        }
        let Some(top) = magnitude.iter().rposition(|&limb| limb != 0) else {
            let zero = if self.added && self.negative_zero {
                -0.0
            } else {
                0.0
            };
            return Some(zero);
        };
        // The position of the highest bit set, counting from 0.
        let highest = top * 64 + 63 - magnitude[top].leading_zeros() as usize;
        let bits = if highest <= 52 {
            // Fewer than 2^53 units: exact, as a subnormal, whose bits are
            // its units, or a number of the least normal exponent, whose
            // bits, with the implicit bit as that exponent, are too.
            magnitude[0]
        } else {
            // The 53 bits from `highest` down, rounded by those below.
            let mut shift = highest - 52;
            let mut significand = bits_at(&magnitude, shift);
            let half = bit(&magnitude, shift - 1);
            let below = (0..shift - 1).any(|i| bit(&magnitude, i));
            if half && (below || significand & 1 == 1) {
                significand += 1;
                if significand == IMPLICIT << 1 {
                    significand >>= 1;
                    shift += 1;
                }
            }
            // The biased exponent is `shift + 1`, the largest finite 0x7FE,
            // and the implicit bit adds the 1.
            if shift + 1 > 0x7FE {
                return None;
            }
            ((shift as u64) << FRACTION_BITS) + significand
        };
        let sign = if negative { 1 << 63 } else { 0 };
        Some(f64::from_bits(sign | bits))
    }
}

/// Bit `i` of the integer `limbs` make, least significant first.
fn bit(limbs: &[u64; LIMBS], i: usize) -> bool {
    limbs[i / 64] >> (i % 64) & 1 == 1
}

/// The 53 bits of the integer `limbs` make from bit `from` up.
fn bits_at(limbs: &[u64; LIMBS], from: usize) -> u64 {
    let (limb, offset) = (from / 64, from % 64);
    let low = limbs[limb] >> offset;
    let high = match (offset, limbs.get(limb + 1)) {
        (0, _) | (_, None) => 0,
        (_, Some(next)) => next << (64 - offset),
    };
    (low | high) & ((IMPLICIT << 1) - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(values: &[f64]) -> Option<f64> {
        let mut sum = ApproximateSum::default();
        for &x in values {
            sum.add(x);
        }
        sum.total()
    }

    /// The sum, which must be the same in every order of `values`: each
    /// rotation, and each reversed.
    fn sum_in_any_order(values: &[f64]) -> Option<f64> {
        let expected = sum(values);
        let mut values = values.to_vec();
        for _ in 0..values.len() {
            values.rotate_left(1);
            let reversed: Vec<f64> = values.iter().rev().copied().collect();
            for order in [sum(&values), sum(&reversed)] {
                assert_eq!(
                    order.map(f64::to_bits),
                    expected.map(f64::to_bits),
                    "{values:?}"
                );
            }
        }
        expected
    }

    #[test]
    fn integers_are_summed_exactly_then_checked() {
        let total = |values: &[i64]| {
            let mut sum = IntegerSum::default();
            for &n in values {
                sum.add(n);
            }
            sum.total()
        };
        assert_eq!(total(&[]), Some(0));
        assert_eq!(total(&[2, -7, 40]), Some(35));
        // The partial sum is out of range in one order, but not the total.
        assert_eq!(total(&[i64::MAX, 1, -1]), Some(i64::MAX));
        assert_eq!(total(&[i64::MAX, 1]), None);
        assert_eq!(total(&[i64::MIN, -1]), None);
        assert_eq!(total(&[i64::MIN, i64::MIN, i64::MAX, i64::MAX, 2]), Some(0));
    }

    #[test]
    fn approximate_numbers_are_summed_exactly_then_rounded_once() {
        assert_eq!(sum(&[]), Some(0.0));
        assert_eq!(sum_in_any_order(&[0.5, 0.25]), Some(0.75));
        // Added one at a time, ten tenths are 0.9999999999999999.
        assert_eq!(sum_in_any_order(&[0.1; 10]), Some(1.0));
        // Added one at a time, from the left, these are 0.
        assert_eq!(sum_in_any_order(&[1e16, 1.0, -1e16]), Some(1.0));
        assert_eq!(sum_in_any_order(&[1e100, 1.0, -1e100, 1e-100]), Some(1.0));
        // 2^53 + 1 is halfway, to the even 2^53; any more is past it.
        let big = 9_007_199_254_740_992.0;
        assert_eq!(sum_in_any_order(&[big, 1.0]), Some(big));
        assert_eq!(sum_in_any_order(&[big, 1.0, 5e-324]), Some(big + 2.0));
        assert_eq!(sum_in_any_order(&[big + 2.0, 1.0]), Some(big + 4.0));
        assert_eq!(sum_in_any_order(&[-big, -1.0, -5e-324]), Some(-big - 2.0));
    }

    #[test]
    fn approximate_sums_are_out_of_range_only_at_the_end() {
        let max = f64::MAX;
        // Added one at a time, from the left, the first two overflow.
        assert_eq!(sum_in_any_order(&[max, max, -max]), Some(max));
        assert_eq!(sum_in_any_order(&[max, max]), None);
        assert_eq!(sum_in_any_order(&[-max, -max]), None);
        // Halfway from the largest to 2^1024, rounded to the even, which
        // is past it, as IEEE 754 addition overflows.
        let half_ulp = 2f64.powi(970);
        assert_eq!(sum_in_any_order(&[max, half_ulp]), None);
        assert_eq!(sum_in_any_order(&[max, half_ulp, -5e-324]), Some(max));
    }

    #[test]
    fn subnormals_and_zeros_are_exact() {
        let tiny = 5e-324;
        assert_eq!(sum_in_any_order(&[tiny, tiny]), Some(1e-323));
        let least_normal = f64::MIN_POSITIVE;
        assert_eq!(
            sum_in_any_order(&[least_normal, -tiny]),
            Some(least_normal - tiny)
        );
        assert_eq!(
            sum_in_any_order(&[least_normal, least_normal]),
            Some(2.0 * least_normal)
        );
        // A sum that is exactly zero is -0.0 only if every value is.
        let is = |total: Option<f64>, zero: f64| total.map(f64::to_bits) == Some(zero.to_bits());
        assert!(is(sum(&[]), 0.0));
        assert!(is(sum_in_any_order(&[-0.0]), -0.0));
        assert!(is(sum_in_any_order(&[-0.0, -0.0]), -0.0));
        assert!(is(sum_in_any_order(&[-0.0, 0.0]), 0.0));
        assert!(is(sum_in_any_order(&[1.5, -1.5, -0.0]), 0.0));
        assert!(is(sum_in_any_order(&[-1.5, 1.5]), 0.0));
    }

    #[test]
    fn one_value_is_itself_and_two_are_their_ieee_sum() {
        let values = [
            0.0,
            1.0,
            -1.0,
            0.1,
            1e300,
            -1e-300,
            5e-324,
            f64::MAX,
            f64::MIN_POSITIVE,
            3.0e-310,
        ];
        for &a in &values {
            assert_eq!(sum(&[a]).map(f64::to_bits), Some(a.to_bits()), "{a}");
            for &b in &values {
                let ieee = a + b;
                let expected = ieee.is_finite().then_some(ieee);
                assert_eq!(
                    sum_in_any_order(&[a, b]).map(f64::to_bits),
                    expected.map(f64::to_bits),
                    "{a} + {b}"
                );
            }
        }
    }
}
