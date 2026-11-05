//! Dates and times written as text, as SQL and most other languages write them,
//! where a value of a type of `trendy.time` is expected: for the fix that selects
//! it. The text only has to have the shape: whether it is a value is the selector's
//! to say, and it says so of the fix as of anything else.

use trendy_hir as hir;

/// The unit that declares the types.
const UNIT: &str = "trendy.time";

/// The selection of a value of `ty`, a type of `trendy.time`, that `text` writes
/// in ISO 8601's form, if it does.
pub(crate) fn selection(ty: &hir::Type, text: &str) -> Option<String> {
    let hir::Type::Defined(defined) = ty else {
        return None;
    };
    let is = |name| hir::Library::is_type_of(&defined.name, UNIT, name);
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
