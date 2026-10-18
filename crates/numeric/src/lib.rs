//! Sums and products of numbers, exact before they are rounded, shared by
//! the compiler (which evaluates `sum` and `product` when compiling) and the
//! runtime (which evaluates them when running), so that the two can't
//! disagree. A relation's tuples have no order, and neither has a sum or a
//! product here: each is the exact result, rounded once, which no order of
//! adding or multiplying can change. It has no dependencies and doesn't
//! need `std`, so both can use it.

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

/// The exact product of integers. A factor of 0 makes it 0, whatever the
/// others; and every other factor is at least 1 in magnitude, so the
/// product only grows in magnitude, and is out of range once a partial
/// product is, beyond 2^63. Until then, it fits in an `i128`, which holds
/// it times another integer too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegerProduct {
    product: i128,
    zero: bool,
    out_of_range: bool,
}

impl Default for IntegerProduct {
    fn default() -> Self {
        IntegerProduct {
            product: 1,
            zero: false,
            out_of_range: false,
        }
    }
}

impl IntegerProduct {
    pub fn multiply(&mut self, n: i64) {
        if n == 0 {
            self.zero = true;
        } else if !self.out_of_range {
            self.product *= i128::from(n);
            self.out_of_range = self.product.unsigned_abs() > 1 << 63;
        }
    }

    /// The product, or `None` if it is out of an integer's range.
    #[must_use]
    pub fn total(self) -> Option<i64> {
        match (self.zero, self.out_of_range) {
            (true, _) => Some(0),
            (false, true) => None,
            (false, false) => i64::try_from(self.product).ok(),
        }
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

/// The finite `x` as `significand` × 2^(`shift` - 1074), `significand`
/// below 2^53: a subnormal's fraction is its count of units of 2^-1074,
/// and a normal number has the implicit bit, at one exponent per shift.
fn decode(x: f64) -> (u64, u16) {
    let bits = x.to_bits();
    // The sign, then 11 bits of biased exponent.
    let [sign_and_high, low, ..] = bits.to_be_bytes();
    let biased = u16::from(sign_and_high & 0x7F) << 4 | u16::from(low >> 4);
    let fraction = bits & (IMPLICIT - 1);
    match biased {
        0 => (fraction, 0),
        _ => (fraction | IMPLICIT, biased - 1),
    }
}

impl ApproximateSum {
    /// Add `x`, which must be finite: an infinity or a NaN has no exact
    /// value, and the total would mean nothing.
    pub fn add(&mut self, x: f64) {
        self.added = true;
        self.negative_zero &= x.to_bits() == (-0.0_f64).to_bits();
        let (significand, shift) = decode(x);
        if significand == 0 {
            return;
        }
        let shift = usize::from(shift);
        let (limb, offset) = (shift / 64, shift % 64);
        let low = significand << offset;
        let high = match offset {
            0 => 0,
            _ => significand >> (64 - offset),
        };
        if x.is_sign_positive() {
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
        let zero = magnitude.iter().all(|&limb| limb == 0);
        nearest(
            &magnitude,
            -1074,
            negative || zero && self.added && self.negative_zero,
        )
    }
}

/// The exact product of finite approximate numbers, rounded once. Their
/// significands are multiplied keeping the highest 128 bits, and their
/// exponents added apart, so that no partial product overflows or
/// underflows. Each step that drops bits makes the result less than the
/// exact product by less than 2^-127 of it, so that the exact product is
/// known to be within a bound above: if that rounds as the result does, so
/// does the exact product, which is otherwise worked out in full
/// ([`Rounded::Exact`]). That takes a product within about 2^-74 of halfway
/// between two approximate numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApproximateProduct {
    /// The highest 128 bits of the product of the significands so far, the
    /// highest one set: the product is about this × 2^`exponent`.
    significand: u128,
    exponent: i64,
    negative: bool,
    /// Whether a factor is zero, which makes the product exactly zero.
    zero: bool,
    /// The number of factors, but zeros, for [`Exact::limbs`].
    factors: u64,
    /// The number of steps that dropped bits that were set.
    truncated: u64,
}

impl Default for ApproximateProduct {
    fn default() -> Self {
        ApproximateProduct {
            significand: 1 << 127,
            exponent: -127,
            negative: false,
            zero: false,
            factors: 0,
            truncated: 0,
        }
    }
}

/// An approximate product rounded once, or what it takes to round it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rounded {
    /// The nearest approximate number, ties to even, or `None` if that is
    /// beyond the largest finite one.
    Nearest(Option<f64>),
    /// Too near halfway between two approximate numbers to tell which
    /// without the exact product.
    Exact(Exact),
}

impl ApproximateProduct {
    /// Multiply by `x`, which must be finite: an infinity or a NaN has no
    /// exact value, and the total would mean nothing.
    pub fn multiply(&mut self, x: f64) {
        self.negative ^= x.is_sign_negative();
        let (significand, shift) = decode(x);
        if significand == 0 {
            self.zero = true;
            return;
        }
        self.factors += 1;
        // Its highest bit set, as the product's is, so that the product of
        // the two is from 2^190 up to 2^192.
        let leading = significand.leading_zeros();
        let factor = significand << leading;
        let exponent = i64::from(shift) - 1074 - i64::from(leading);
        let (low, high) = halves(self.significand);
        let (p0, low_high) = halves(u128::from(low) * u128::from(factor));
        let (high_low, high_high) = halves(u128::from(high) * u128::from(factor));
        let (p1, carry) = low_high.overflowing_add(high_low);
        let p2 = high_high + u64::from(carry);
        // The highest 128 bits of the 192, and those below them.
        let (significand, dropped, shift) = if p2 >> 63 == 1 {
            (join(p1, p2), p0, 64)
        } else {
            (join(p1 << 1 | p0 >> 63, p2 << 1 | p1 >> 63), p0 << 1, 63)
        };
        self.significand = significand;
        self.exponent += exponent + shift;
        self.truncated += u64::from(dropped != 0);
    }

    /// The product, rounded once, if the highest 128 bits tell it. Of no
    /// factors, it is `1.0`.
    #[must_use]
    pub fn total(&self) -> Rounded {
        if self.zero {
            return Rounded::Nearest(Some(if self.negative { -0.0 } else { 0.0 }));
        }
        let (low, high) = halves(self.significand);
        let rounded = nearest(&[low, high], self.exponent, self.negative);
        if self.truncated == 0 {
            return Rounded::Nearest(rounded);
        }
        // Each truncation leaves the result below the exact product by
        // less than 2^-127 of it, and so all of them, by less than
        // 2^-126 of it per truncation, which, of a result below 2^128
        // units, is less than 4 units per truncation.
        let (slack_low, slack_high) = halves(4 * u128::from(self.truncated));
        let (above_low, carry) = low.overflowing_add(slack_low);
        let (above_high, carry_high) = high.carrying_add(slack_high, carry);
        let above = [above_low, above_high, u64::from(carry_high)];
        let bound = nearest(&above, self.exponent, self.negative);
        if bound.map(f64::to_bits) == rounded.map(f64::to_bits) {
            Rounded::Nearest(rounded)
        } else {
            Rounded::Exact(Exact {
                factors: self.factors,
                negative: self.negative,
            })
        }
    }
}

/// Working out an approximate product exactly, which takes the factors
/// again, and memory for [`Exact::limbs`] 64-bit limbs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exact {
    factors: u64,
    negative: bool,
}

impl Exact {
    /// The memory the exact product takes, in 64-bit limbs: each factor's
    /// significand has at most 53 bits. Beyond what can be addressed, it is
    /// `usize::MAX`, which no allocation has.
    #[must_use]
    pub fn limbs(&self) -> usize {
        let bits = self.factors.saturating_mul(53);
        usize::try_from(bits / 64 + 2).unwrap_or(usize::MAX)
    }

    /// The exact product of `factors`, the ones multiplied before, rounded
    /// once, worked out in `memory`, of at least [`Exact::limbs`] limbs.
    /// It takes time quadratic in the number of factors.
    ///
    /// # Panics
    ///
    /// If `memory` is shorter than that.
    #[must_use]
    pub fn total(&self, factors: impl IntoIterator<Item = f64>, memory: &mut [u64]) -> Option<f64> {
        assert!(memory.len() >= self.limbs(), "too little memory");
        memory[0] = 1;
        let mut len = 1;
        let mut exponent = 0;
        for factor in factors {
            let (significand, shift) = decode(factor);
            if significand == 0 {
                continue;
            }
            // Without its trailing zeros, which only add to the exponent.
            let zeros = significand.trailing_zeros();
            let significand = significand >> zeros;
            exponent += i64::from(shift) - 1074 + i64::from(zeros);
            let mut carry = 0;
            for limb in &mut memory[..len] {
                (*limb, carry) =
                    halves(u128::from(*limb) * u128::from(significand) + u128::from(carry));
            }
            if carry != 0 {
                memory[len] = carry;
                len += 1;
            }
        }
        nearest(&memory[..len], exponent, self.negative)
    }
}

/// The low and high halves of `n`.
fn halves(n: u128) -> (u64, u64) {
    let [
        l0,
        l1,
        l2,
        l3,
        l4,
        l5,
        l6,
        l7,
        h0,
        h1,
        h2,
        h3,
        h4,
        h5,
        h6,
        h7,
    ] = n.to_le_bytes();
    (
        u64::from_le_bytes([l0, l1, l2, l3, l4, l5, l6, l7]),
        u64::from_le_bytes([h0, h1, h2, h3, h4, h5, h6, h7]),
    )
}

/// The number whose halves are `low` and `high`.
fn join(low: u64, high: u64) -> u128 {
    u128::from(high) << 64 | u128::from(low)
}

/// The approximate number nearest `magnitude` × 2^`exponent`, ties to even,
/// with the sign `negative`, or `None` if that is beyond the largest finite
/// one. `magnitude` is an integer, least significant limb first. A result
/// too small for a subnormal is zero, of the sign, as IEEE 754 rounds it.
fn nearest(magnitude: &[u64], exponent: i64, negative: bool) -> Option<f64> {
    let sign = u64::from(negative) << 63;
    let zero = Some(f64::from_bits(sign));
    // The positions of the lowest and highest bits set, counting from 0. A
    // position beyond an `i64` is of a number far beyond any finite one.
    let Some(top) = magnitude.iter().rposition(|&limb| limb != 0) else {
        return zero;
    };
    let bottom = magnitude.iter().position(|&limb| limb != 0)?;
    let position = |limb: usize, bit: u32| Some(i64::try_from(limb).ok()? * 64 + i64::from(bit));
    let highest = position(top, magnitude[top].ilog2())?;
    let lowest = position(bottom, magnitude[bottom].trailing_zeros())?;
    // Bits below `cut` are rounded off: those after the 53 from the
    // highest, or those below 2^-1074, the least subnormal.
    let cut = (highest - 52).max(-1074 - exponent);
    let mut significand = if cut <= 0 {
        // All within 53 bits, so within the first limb.
        magnitude[0] << -cut
    } else {
        let bit = |i: i64| {
            usize::try_from(i)
                .ok()
                .and_then(|i| magnitude.get(i / 64).map(|limb| limb >> (i % 64) & 1 == 1))
                .unwrap_or(false)
        };
        let kept = (0..53).fold(0, |kept, i| kept | u64::from(bit(cut + i)) << i);
        // Up if past halfway, or halfway and odd.
        let halfway = bit(cut - 1);
        let past = lowest < cut - 1;
        kept + u64::from(halfway && (past || kept & 1 == 1))
    };
    let mut scale = cut + exponent;
    if significand == IMPLICIT << 1 {
        significand = IMPLICIT;
        scale += 1;
    }
    let bits = if significand >= IMPLICIT {
        // A normal number, whose biased exponent is `scale + 1075`, the
        // largest finite 0x7FE, and the implicit bit adds the 1.
        if scale + 1075 > 0x7FE {
            return None;
        }
        ((scale + 1074).unsigned_abs() << FRACTION_BITS) + significand
    } else {
        // A subnormal, or zero, in units of 2^-1074.
        significand
    };
    Some(f64::from_bits(sign | bits))
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

    fn product(values: &[f64]) -> Option<f64> {
        let mut product = ApproximateProduct::default();
        for &x in values {
            product.multiply(x);
        }
        match product.total() {
            Rounded::Nearest(total) => total,
            Rounded::Exact(exact) => {
                exact.total(values.iter().copied(), &mut vec![0; exact.limbs()])
            }
        }
    }

    /// The product, worked out in full.
    fn exact_product(values: &[f64]) -> Option<f64> {
        let mut product = ApproximateProduct::default();
        for &x in values {
            product.multiply(x);
        }
        if product.zero {
            return Some(if product.negative { -0.0 } else { 0.0 });
        }
        let exact = Exact {
            factors: product.factors,
            negative: product.negative,
        };
        exact.total(values.iter().copied(), &mut vec![0; exact.limbs()])
    }

    /// The product, which must be the same in every order of `values`, and
    /// worked out in full.
    fn product_in_any_order(values: &[f64]) -> Option<f64> {
        let expected = product(values);
        assert_eq!(
            exact_product(values).map(f64::to_bits),
            expected.map(f64::to_bits),
            "{values:?}"
        );
        let mut values = values.to_vec();
        for _ in 0..values.len() {
            values.rotate_left(1);
            let reversed: Vec<f64> = values.iter().rev().copied().collect();
            for order in [product(&values), product(&reversed)] {
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
    fn integers_are_multiplied_exactly_then_checked() {
        let total = |values: &[i64]| {
            let mut product = IntegerProduct::default();
            for &n in values {
                product.multiply(n);
            }
            product.total()
        };
        assert_eq!(total(&[]), Some(1));
        assert_eq!(total(&[2, 3, -4]), Some(-24));
        // Out of range on the way in one order, but zero.
        assert_eq!(total(&[i64::MAX, 2, 0]), Some(0));
        assert_eq!(total(&[0, i64::MAX, 2]), Some(0));
        // 2^63 is out of range, but -2^63 isn't.
        let half = 1 << 62;
        assert_eq!(total(&[half, 2]), None);
        assert_eq!(total(&[half, 2, -1]), Some(i64::MIN));
        assert_eq!(total(&[-1, half, 2]), Some(i64::MIN));
        assert_eq!(total(&[i64::MIN, -1]), None);
        assert_eq!(total(&[i64::MIN, 1]), Some(i64::MIN));
    }

    #[test]
    fn approximate_numbers_are_multiplied_exactly_then_rounded_once() {
        assert_eq!(product(&[]), Some(1.0));
        assert_eq!(product_in_any_order(&[1.5, -2.0, 0.25]), Some(-0.75));
        // Out of range on the way in one order, but not in the end.
        let max = f64::MAX;
        assert_eq!(product_in_any_order(&[max, 2.0, 0.5]), Some(max));
        assert_eq!(product_in_any_order(&[max, 2.0]), None);
        assert_eq!(product_in_any_order(&[-max, 2.0]), None);
        let near_one = product_in_any_order(&[1e200, 1e200, 1e-200, 1e-200]).unwrap();
        assert!((near_one - 1.0).abs() < 1e-15, "{near_one}");
        // Below the least subnormal on the way, but not in the end: 5e-324
        // is 2^-1074, so `1e300 * 5e-324` is exact, and the product of two
        // of them is rounded once.
        let scaled = 1e300 * 5e-324;
        assert_eq!(
            product_in_any_order(&[5e-324, 5e-324, 1e300, 1e300]),
            Some(scaled * scaled)
        );
        // Three times the double nearest a third is exactly halfway below 1,
        // to the even 1.
        assert_eq!(product_in_any_order(&[3.0, 1.0 / 3.0]), Some(1.0));
        assert_eq!(product_in_any_order(&[3.0, 1.0 / 3.0, -1.0]), Some(-1.0));
    }

    #[test]
    fn approximate_products_round_into_subnormals_and_zero() {
        let tiny = 5e-324;
        // Halfway to the least subnormal, to the even 0; past it, to it;
        // halfway between one and two units, to the even two.
        assert_eq!(
            product_in_any_order(&[tiny, 0.5]).map(f64::to_bits),
            Some(0)
        );
        assert_eq!(product_in_any_order(&[tiny, 0.75]), Some(tiny));
        assert_eq!(product_in_any_order(&[tiny, 1.5]), Some(2.0 * tiny));
        assert_eq!(
            product_in_any_order(&[-tiny, 0.25]).map(f64::to_bits),
            Some((-0.0_f64).to_bits())
        );
        // A zero factor makes the product zero, of the sign of the rest.
        let is = |total: Option<f64>, zero: f64| total.map(f64::to_bits) == Some(zero.to_bits());
        assert!(is(product_in_any_order(&[-0.0, 5.0]), -0.0));
        assert!(is(product_in_any_order(&[-0.0, -5.0]), 0.0));
        assert!(is(product_in_any_order(&[0.0, f64::MAX, 2.0]), 0.0));
    }

    #[test]
    fn one_factor_is_itself_and_two_are_their_ieee_product() {
        let values = [
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.1,
            3.0,
            1e300,
            -1e-300,
            5e-324,
            f64::MAX,
            f64::MIN_POSITIVE,
            3.0e-310,
            1.0 / 3.0,
            0.7,
        ];
        for &a in &values {
            assert_eq!(product(&[a]).map(f64::to_bits), Some(a.to_bits()), "{a}");
            for &b in &values {
                let ieee = a * b;
                let expected = ieee.is_finite().then_some(ieee);
                assert_eq!(
                    product_in_any_order(&[a, b]).map(f64::to_bits),
                    expected.map(f64::to_bits),
                    "{a} * {b}"
                );
            }
        }
    }

    #[test]
    fn the_highest_128_bits_agree_with_the_exact_product() {
        // Products of many factors, from a fixed linear congruential
        // sequence of bits, of all magnitudes, keep only the highest 128
        // bits of the product; worked out in full, each rounds the same.
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state
        };
        for count in 1..40 {
            let factors: Vec<f64> = (0..count)
                .map(|_| {
                    // Exponents near 0, so that most products are finite.
                    let bits = next() & !(0x7FF << 52) | (1023 - 20 + next() % 40) << 52;
                    f64::from_bits(bits)
                })
                .collect();
            product_in_any_order(&factors);
        }
    }

    #[test]
    fn a_product_too_near_halfway_is_worked_out_in_full() {
        // The highest 128 bits of a product halfway between 1 and the next
        // approximate number, 1 + 2^-52, after a truncation: the exact
        // product is above them by up to 4 units, so either way.
        let halfway = ApproximateProduct {
            significand: 1 << 127 | 1 << 74,
            exponent: -127,
            negative: false,
            zero: false,
            factors: 3,
            truncated: 1,
        };
        assert!(matches!(halfway.total(), Rounded::Exact(_)));
        // Untruncated, it is exactly halfway, to the even 1.
        let exact = ApproximateProduct {
            truncated: 0,
            ..halfway
        };
        assert_eq!(exact.total(), Rounded::Nearest(Some(1.0)));
        // Far from halfway, the bound doesn't matter.
        let far = ApproximateProduct {
            significand: 1 << 127 | 1 << 100,
            ..halfway
        };
        assert_eq!(far.total(), Rounded::Nearest(Some(1.0 + 2f64.powi(-27))));
    }
}
