//! Dates and times written as other languages write them: as text, where a value of a
//! type of `trendy.time` is expected, for the fix that selects it, and with `+` and `-`,
//! for the fix that calls its operators. Text only has to have the shape: whether it is a
//! value is the selector's to say, and it says so of the fix as of anything else.

use trendy_diag::Edit;
use trendy_hir as hir;
use trendy_syntax::ast::Expr;

use crate::ops::{OnTheirOwn, Operator};

/// The unit that declares the types.
const UNIT: &str = "trendy.time";

/// The selection of a value of `ty`, a type of `trendy.time`, that `text` writes
/// in ISO 8601's form, if it does.
pub(crate) fn selection(ty: &hir::Type, text: &str) -> Option<String> {
    let hir::Type::Defined(defined) = ty else {
        return None;
    };
    let is = |name| defined.is_type_of(UNIT, name);
    // As the type's own name is qualified, so are the others'.
    let qualified = defined
        .name
        .rsplit_once("::")
        .map_or("", |(_, _)| "trendy.time::");
    if is("date") {
        let (year, month, day) = date(text)?;
        return Some(format!(
            "{qualified}date{{year: {year}, month: {month}, day: {day}}}"
        ));
    }
    if is("time") {
        return clock(text).map(|clock| clock_selection(qualified, clock));
    }
    if is("duration") {
        let (seconds, subsec_nanos) = duration(text)?;
        return Some(format!(
            "{qualified}duration{{seconds: {seconds}, subsec_nanos: {subsec_nanos}}}"
        ));
    }
    let (on, at) = text.split_once(['T', ' '])?;
    let (year, month, day) = date(on)?;
    if is("timestamp") {
        // An instant in UTC, as the type is, unless the text says otherwise.
        let at = at.strip_suffix('Z').unwrap_or(at);
        return Some(format!(
            "{qualified}timestamp{{date: {qualified}date{{year: {year}, month: {month}, day: {day}}}, time: {}}}",
            clock_selection(qualified, clock(at)?)
        ));
    }
    if is("offset_timestamp") {
        let (at, utc_offset) = offset(at)?;
        let (hour, minute, second, nanosecond) = clock(at)?;
        return Some(format!(
            "{qualified}offset_timestamp{{year: {year}, month: {month}, day: {day}, hour: {hour}, minute: {minute}, second: {second}, nanosecond: {nanosecond}, utc_offset: {utc_offset}}}"
        ));
    }
    None
}

/// What `operands`, each as `own` has it, of `op`, mean if they are `t - s` of two
/// instants, or `t + d` or `t - d` of an instant and a duration: what to say of the fix, and
/// the fix, calling the operator that has that value.
pub(crate) fn arithmetic(
    op: Operator,
    operands: &[Option<Expr>],
    own: &OnTheirOwn,
) -> Option<(String, Edit)> {
    let [Some(lhs), Some(rhs)] = operands else {
        return None;
    };
    let types: Vec<&hir::Type> = own.types().collect();
    let [hir::Type::Defined(left), hir::Type::Defined(right)] = types.as_slice() else {
        return None;
    };
    let is = |defined: &hir::Defined, name| defined.is_type_of(UNIT, name);
    let call = match (
        op,
        is(left, "timestamp"),
        is(right, "timestamp"),
        is(left, "duration"),
        is(right, "duration"),
    ) {
        (Operator::Subtract, true, true, ..) => {
            format!("elapsed{{from: {}, to: {}}}", text(rhs), text(lhs))
        }
        (Operator::Add, true, _, _, true) => {
            format!("after{{t: {}, by: {}}}", text(lhs), text(rhs))
        }
        (Operator::Add, _, true, true, _) => {
            format!("after{{t: {}, by: {}}}", text(rhs), text(lhs))
        }
        (Operator::Subtract, true, _, _, true) => {
            format!("before{{t: {}, by: {}}}", text(lhs), text(rhs))
        }
        _ => return None,
    };
    let replacement = match left.name.rsplit_once("::") {
        Some(_) => format!("{UNIT}::{call}"),
        None => call,
    };
    let span = trendy_diag::Span {
        start: lhs.span().start,
        end: rhs.span().end,
    };
    Some((
        format!("`trendy.time` has no `+` or `-`: write `{replacement}`"),
        Edit { span, replacement },
    ))
}

/// How to do what arithmetic on `defined` would, if it is a type of `trendy.time` with no
/// one number to compute with.
pub(crate) fn arithmetic_help(defined: &hir::Defined) -> Option<String> {
    let is = |name| defined.is_type_of(UNIT, name);
    if is("timestamp") {
        return Some(
            "`after{t, by: seconds{n: 1}}` and `before{t, by: …}` move a `timestamp` by a `duration`, and `elapsed{from, to}` is the `duration` between two"
                .to_string(),
        );
    }
    is("duration").then(|| {
        "a `duration` has no arithmetic yet: `elapsed{from, to}` gives one, `after{t, by}` and `before{t, by}` move a `timestamp` by one, and its `seconds` and `subsec_nanos` can be computed with"
            .to_string()
    })
}

/// The text of `expr`, as it is written.
fn text(expr: &Expr) -> String {
    expr.syntax().text().to_string()
}

/// The selection of the time `clock`.
fn clock_selection(
    qualified: &str,
    (hour, minute, second, nanosecond): (u32, u32, u32, u32),
) -> String {
    format!(
        "{qualified}time{{hour: {hour}, minute: {minute}, second: {second}, nanosecond: {nanosecond}}}"
    )
}

/// `digits`, of exactly `width` of them, as a number.
fn number(digits: &str, width: usize) -> Option<u32> {
    (digits.len() == width && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// The year, month and day of `YYYY-MM-DD`.
fn date(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('-');
    let found = (
        number(parts.next()?, 4)?,
        number(parts.next()?, 2)?,
        number(parts.next()?, 2)?,
    );
    parts.next().is_none().then_some(found)
}

/// The hour, minute, second and nanosecond of `HH:MM:SS` or `HH:MM:SS.fraction`, of up
/// to 9 digits.
fn clock(text: &str) -> Option<(u32, u32, u32, u32)> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, "0"));
    let mut parts = whole.split(':');
    let (hour, minute, second) = (
        number(parts.next()?, 2)?,
        number(parts.next()?, 2)?,
        number(parts.next()?, 2)?,
    );
    if parts.next().is_some() || fraction.is_empty() || fraction.len() > 9 {
        return None;
    }
    let nanosecond =
        number(fraction, fraction.len())? * 10_u32.pow(9 - u32::try_from(fraction.len()).ok()?);
    Some((hour, minute, second, nanosecond))
}

/// The seconds, rounded down, and the nanoseconds past them, of ISO 8601's `PnDTnHnMnS`,
/// each part optional, as long as there is one, and with `-` before it for that long back.
/// Its years and months have no fixed length, so a duration has none.
fn duration(text: &str) -> Option<(i64, u32)> {
    let (sign, written) = match text.strip_prefix('-') {
        Some(written) => (-1, written),
        None => (1, text),
    };
    let written = written.strip_prefix('P')?;
    let (date_part, clock) = written.split_once('T').unwrap_or((written, ""));
    let (days, rest) = part(date_part, 'D');
    if !rest.is_empty() || written.ends_with('T') {
        return None;
    }
    let (hours, clock) = part(clock, 'H');
    let (minutes, clock) = part(clock, 'M');
    let (seconds, rest) = part(clock, 'S');
    if !rest.is_empty() {
        return None;
    }
    let (seconds, fraction) = match seconds.map(|s| s.split_once('.').unwrap_or((s, "0"))) {
        Some((seconds, fraction)) => (Some(seconds), fraction),
        None => (None, "0"),
    };
    let width = u32::try_from(fraction.len())
        .ok()
        .filter(|w| (1..=9).contains(w))?;
    if days.is_none() && hours.is_none() && minutes.is_none() && seconds.is_none() {
        return None;
    }
    let total = sign
        * ((whole_or_zero(days)? * 86_400
            + whole_or_zero(hours)? * 3_600
            + whole_or_zero(minutes)? * 60
            + whole_or_zero(seconds)?)
            * 1_000_000_000
            + whole(fraction)? * 10_i128.pow(9 - width));
    Some((
        i64::try_from(total.div_euclid(1_000_000_000)).ok()?,
        u32::try_from(total.rem_euclid(1_000_000_000)).ok()?,
    ))
}

/// The number before `unit` at the start of `text`, as it was written, if `unit` is there,
/// and what comes after it.
fn part(text: &str, unit: char) -> (Option<&str>, &str) {
    match text.split_once(unit) {
        Some((number, rest)) => (Some(number), rest),
        None => (None, text),
    }
}

/// `written` as a number, if it is one, or 0 if nothing is written.
fn whole_or_zero(written: Option<&str>) -> Option<i128> {
    written.map_or(Some(0), whole)
}

/// `digits` as a number, of not so many digits that it can overflow.
fn whole(digits: &str) -> Option<i128> {
    (!digits.is_empty() && digits.len() <= 20 && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// What is before the offset at the end of `text`, and the offset, in seconds: `Z`,
/// `+05:45` or `-05:00`.
fn offset(text: &str) -> Option<(&str, i32)> {
    if let Some(at) = text.strip_suffix('Z') {
        return Some((at, 0));
    }
    let split = text.rfind(['+', '-'])?;
    let (at, written) = text.split_at(split);
    let sign = if written.starts_with('-') { -1 } else { 1 };
    let (hours, minutes) = written[1..].split_once(':')?;
    let seconds = i32::try_from(number(hours, 2)? * 3600 + number(minutes, 2)? * 60).ok()?;
    Some((at, sign * seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_8601_durations_are_read_by_their_shape() {
        for (written, right) in [
            ("PT5M", (300, 0)),
            ("P1D", (86_400, 0)),
            ("P1DT1H", (90_000, 0)),
            ("PT1H30M5S", (5_405, 0)),
            ("PT0.5S", (0, 500_000_000)),
            ("PT1.000000001S", (1, 1)),
            // Rounded down, as a `duration` is.
            ("-PT0.5S", (-1, 500_000_000)),
            ("-PT5M", (-300, 0)),
            ("PT0S", (0, 0)),
        ] {
            assert_eq!(duration(written), Some(right), "{written}");
        }
        // Years and months have no fixed length, and nothing else is one.
        for wrong in [
            "P1Y",
            "P1M",
            "P1Y2M3D",
            "P",
            "PT",
            "5M",
            "PT5",
            "PTS",
            "PT1.S",
            "PT0.1234567891S",
            "PT5S5M",
            "P1DT",
            "5 minutes",
            "",
        ] {
            assert_eq!(duration(wrong), None, "{wrong}");
        }
        // What can't be a number of seconds is no value.
        assert_eq!(duration("PT99999999999999999999S"), None);
    }

    #[test]
    fn iso_8601_is_read_by_its_shape() {
        assert_eq!(date("2024-03-05"), Some((2024, 3, 5)));
        // Whether it is a date is the selector's to say.
        assert_eq!(date("2024-02-30"), Some((2024, 2, 30)));
        for wrong in [
            "2024-3-5",
            "24-03-05",
            "2024-03",
            "2024-03-05-01",
            "March 5",
            "",
        ] {
            assert_eq!(date(wrong), None, "{wrong}");
        }
        assert_eq!(clock("13:07:09"), Some((13, 7, 9, 0)));
        assert_eq!(clock("13:07:09.5"), Some((13, 7, 9, 500_000_000)));
        assert_eq!(clock("13:07:09.123456789"), Some((13, 7, 9, 123_456_789)));
        for wrong in [
            "13:07",
            "13:07:09.",
            "13:07:09.1234567891",
            "1:07:09",
            "13:07:09:00",
        ] {
            assert_eq!(clock(wrong), None, "{wrong}");
        }
        assert_eq!(offset("13:07:09Z"), Some(("13:07:09", 0)));
        assert_eq!(offset("13:07:09+05:45"), Some(("13:07:09", 20_700)));
        assert_eq!(offset("13:07:09-05:00"), Some(("13:07:09", -18_000)));
        assert_eq!(offset("13:07:09"), None);
        assert_eq!(offset("13:07:09+0545"), None);
    }
}
