//! Parsers for the challenge's restricted CSV and timestamp formats.

/// Split one physical line (without its LF). CR, spaces and UTF-8 bytes are
/// ordinary field content. Empty fields are represented by empty slices.
// Most event records fit in two 64-bit masks. Index their commas and quotes
// together in one pass. Arbitrarily long records take the scalar path.
#[inline]
pub fn fields<const N: usize>(line: &[u8]) -> Option<[&[u8]; N]> {
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    if N > 0 && (16..128).contains(&line.len()) {
        #[cfg(target_arch = "x86_64")]
        if line.len() >= 32 && is_x86_feature_detected!("avx2") {
            // Feature detection and the outer length check cover all vector loads.
            return unsafe { indexed_fields_avx2(line) };
        }
        return indexed_fields(line);
    }
    fields_scalar(line)
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn punctuation(block: &[u8; 16]) -> (u16, u16) {
    use std::arch::x86_64::*;
    // SSE2 is available on x86-64. block contains at least 16 readable bytes.
    unsafe {
        let bytes = _mm_loadu_si128(block.as_ptr().cast());
        let commas = _mm_cmpeq_epi8(bytes, _mm_set1_epi8(b',' as i8));
        let quotes = _mm_cmpeq_epi8(bytes, _mm_set1_epi8(b'"' as i8));
        (
            _mm_movemask_epi8(commas) as u16,
            _mm_movemask_epi8(quotes) as u16,
        )
    }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn punctuation(block: &[u8; 16]) -> (u16, u16) {
    use std::arch::aarch64::*;
    const WEIGHTS: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
    #[inline(always)]
    unsafe fn mask(lanes: uint8x16_t, weights: uint8x16_t) -> u16 {
        // NEON is enabled for the supported aarch64 Linux and macOS targets.
        unsafe {
            let bits = vandq_u8(lanes, weights);
            u16::from(vaddv_u8(vget_low_u8(bits))) | (u16::from(vaddv_u8(vget_high_u8(bits))) << 8)
        }
    }
    unsafe {
        let bytes = vld1q_u8(block.as_ptr());
        let weights = vld1q_u8(WEIGHTS.as_ptr());
        (
            mask(vceqq_u8(bytes, vdupq_n_u8(b',')), weights),
            mask(vceqq_u8(bytes, vdupq_n_u8(b'"')), weights),
        )
    }
}

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
#[inline(always)]
fn indexed_fields<const N: usize>(line: &[u8]) -> Option<[&[u8]; N]> {
    let mut commas = 0_u128;
    let mut quotes = 0_u128;
    let mut offset = 0;
    while offset + 16 <= line.len() {
        let (c, q) = punctuation(line[offset..offset + 16].try_into().unwrap());
        commas |= u128::from(c) << offset;
        quotes |= u128::from(q) << offset;
        offset += 16;
    }
    if offset < line.len() {
        // The final full-width load overlaps the previous block. OR preserves
        // all already-discovered punctuation and never reads past the input.
        let offset = line.len() - 16;
        let (c, q) = punctuation(line[offset..].try_into().unwrap());
        commas |= u128::from(c) << offset;
        quotes |= u128::from(q) << offset;
    }
    unpack_fields(line, commas, quotes)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn indexed_fields_avx2<const N: usize>(line: &[u8]) -> Option<[&[u8]; N]> {
    use std::arch::x86_64::*;
    let mut commas = 0_u128;
    let mut quotes = 0_u128;
    let comma = _mm256_set1_epi8(b',' as i8);
    let quote = _mm256_set1_epi8(b'"' as i8);
    let mut mask = |offset: usize| {
        // The final unaligned block may overlap the previous one. Every load
        // is within the slice, and OR is idempotent on the overlapping bytes.
        let block: &[u8; 32] = line[offset..offset + 32].try_into().unwrap();
        let data = unsafe { _mm256_loadu_si256(block.as_ptr().cast()) };
        let c = _mm256_movemask_epi8(_mm256_cmpeq_epi8(data, comma)) as u32;
        let q = _mm256_movemask_epi8(_mm256_cmpeq_epi8(data, quote)) as u32;
        commas |= u128::from(c) << offset;
        quotes |= u128::from(q) << offset;
    };
    let last = line.len() - 32;
    for offset in (0..last).step_by(32) {
        mask(offset);
    }
    mask(last);
    unpack_fields(line, commas, quotes)
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[inline(always)]
fn unpack_fields<const N: usize>(
    line: &[u8],
    mut commas: u128,
    quotes: u128,
) -> Option<[&[u8]; N]> {
    if commas.count_ones() as usize + 1 != N {
        return None;
    }
    let last_byte = 1_u128 << (line.len() - 1);
    let field_edges = (commas << 1) | (commas >> 1) | 1 | last_byte;
    if quotes & !field_edges != 0 {
        return None;
    }

    commas |= last_byte << 1;
    let mut out = [&[][..]; N];
    let mut start = 0;
    for slot in &mut out {
        let end = commas.trailing_zeros() as usize;
        commas &= commas - 1;
        let raw = &line[start..end];
        let leading = usize::from(raw.first() == Some(&b'"'));
        let trailing = usize::from(raw.last() == Some(&b'"'));
        if leading != trailing || raw.len() < 2 * leading {
            return None;
        }
        *slot = &raw[leading..raw.len() - trailing];
        start = end + 1;
    }
    Some(out)
}

#[inline]
fn fields_scalar<const N: usize>(line: &[u8]) -> Option<[&[u8]; N]> {
    if N == 0 || line.is_empty() {
        return None;
    }
    let mut out = [&[][..]; N];
    let mut start = 0;
    let mut commas = memchr::memchr_iter(b',', line);
    for (index, slot) in out.iter_mut().enumerate() {
        let end = if index + 1 == N {
            if commas.next().is_some() {
                return None;
            }
            line.len()
        } else {
            commas.next()?
        };
        let raw = &line[start..end];
        let value = if raw.first() == Some(&b'"') {
            raw[1..].strip_suffix(b"\"")?
        } else {
            raw
        };
        if value.contains(&b'"') {
            return None;
        }
        *slot = value;
        start = end + 1;
    }
    Some(out)
}

// Days since January 1 of year zero for the start of each month in a
// Gregorian cycle, plus the end of the cycle. Both date validation and
// conversion use this table. Common calendar dates occupy a few cache lines.
static MONTH_STARTS: [i32; 4801] = {
    let mut table = [0; 4801];
    let mut index = 0;
    while index < 4800 {
        let year = index / 12;
        let days = match index % 12 {
            1 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
            1 => 28,
            3 | 5 | 8 | 10 => 30,
            _ => 31,
        };
        table[index + 1] = table[index] + days;
        index += 1;
    }
    table
};

#[inline(always)]
fn pair(bytes: &[u8], at: usize) -> Option<u32> {
    let high = bytes[at].wrapping_sub(b'0');
    let low = bytes[at + 1].wrapping_sub(b'0');
    (high < 10 && low < 10).then(|| u32::from(high) * 10 + u32::from(low))
}

#[inline]
fn calendar(ts: &[u8]) -> Option<i64> {
    let len = ts.len();
    if (len != 20 && len != 25)
        || ts[7] != b'-'
        || ts[10] != b'T'
        || ts[13] != b':'
        || ts[16] != b':'
    {
        return None;
    }
    let year = pair(ts, 0)? * 100 + pair(ts, 2)?;
    let month = pair(ts, 5)?.wrapping_sub(1);
    let day = pair(ts, 8)?.wrapping_sub(1);
    let hour = pair(ts, 11)?;
    let minute = pair(ts, 14)?;
    let second = pair(ts, 17)?;
    if month >= 12 || hour >= 24 || minute >= 60 || second >= 60 {
        return None;
    }
    let index = (year % 400 * 12 + month) as usize;
    let start = MONTH_STARTS[index];
    if day >= (MONTH_STARTS[index + 1] - start) as u32 {
        return None;
    }
    let days = i64::from(year / 400) * 146_097 + i64::from(start) + i64::from(day) - 719_528;
    let offset_seconds = if len == 20 {
        if ts[19] != b'Z' {
            return None;
        }
        0
    } else {
        let hours = pair(ts, 20)?;
        let minutes = pair(ts, 23)?;
        if hours >= 24 || minutes >= 60 || ts[22] != b':' {
            return None;
        }
        let offset = i64::from(hours * 3600 + minutes * 60);
        match ts[19] {
            b'+' => offset,
            b'-' => -offset,
            _ => return None,
        }
    };
    Some((days * 86_400 + i64::from(hour * 3600 + minute * 60 + second) - offset_seconds) * 1000)
}

/// The schema guarantees that every ingest_ts is valid, whole-second UTC F1.
/// Keep the epoch calculation identical to timestamp(), including dates
/// earlier than the Unix epoch.
#[inline]
pub fn ingest_timestamp(ts: &[u8]) -> Option<i64> {
    if ts.len() != 20 {
        return timestamp(ts);
    }
    #[inline(always)]
    fn decimal_pair(input: &[u8], offset: usize) -> u32 {
        u32::from(input[offset] & 15) * 10 + u32::from(input[offset + 1] & 15)
    }
    let year = decimal_pair(ts, 0) * 100 + decimal_pair(ts, 2);
    let month = decimal_pair(ts, 5);
    if !(1..=12).contains(&month) {
        return None;
    }
    let index = (year % 400 * 12 + month - 1) as usize;
    let days = i64::from(year / 400) * 146_097
        + i64::from(MONTH_STARTS[index])
        + i64::from(decimal_pair(ts, 8))
        - 719_529;
    let seconds = decimal_pair(ts, 11) * 3600 + decimal_pair(ts, 14) * 60 + decimal_pair(ts, 17);
    Some((days * 86_400 + i64::from(seconds)) * 1000)
}

/// Parse UTC milliseconds. Numeric epochs beyond i64::MAX cannot fit inside
/// a manifest's four-digit-year window, so they also return None.
#[inline]
pub fn timestamp(ts: &[u8]) -> Option<i64> {
    if ts.get(4) == Some(&b'-') {
        return calendar(ts);
    }
    if ts.is_empty() {
        return None;
    }
    let mut value = 0_i64;
    if ts.len() <= 18 {
        for &byte in ts {
            let digit = byte.wrapping_sub(b'0');
            if digit > 9 {
                return None;
            }
            value = value * 10 + i64::from(digit);
        }
    } else {
        for &byte in ts {
            let digit = byte.wrapping_sub(b'0');
            if digit > 9 {
                return None;
            }
            value = value.checked_mul(10)?.checked_add(i64::from(digit))?;
        }
    }
    Some(value)
}

#[inline]
pub fn price(bytes: &[u8]) -> Option<u32> {
    let mut value = 0;
    for &byte in bytes {
        let digit = byte.wrapping_sub(b'0');
        if digit > 9 {
            return None;
        }
        // The previous prefix was at most 10,000,000, so this fits in u32.
        value = value * 10 + u32::from(digit);
        if value > 10_000_000 {
            return None;
        }
    }
    (value != 0).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_bytes_and_quotes() {
        assert_eq!(
            fields::<3>(b"\" a\",\"\",x\r"),
            Some([&b" a"[..], &b""[..], &b"x\r"[..]])
        );
        assert_eq!(fields::<2>(b","), Some([&b""[..], &b""[..]]));
        for line in [
            b"\"a,b\"".as_slice(),
            b"x,\"",
            b"x,\"\"\"",
            b"b,b\"b",
            b"a,b,c",
            b"b",
            b"",
            b"x,\"a\"\r",
        ] {
            assert_eq!(fields::<2>(line), None, "{line:?}");
        }
    }

    #[test]
    fn timestamp_boundaries_and_offsets() {
        assert_eq!(timestamp(b"1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(timestamp(b"1969-12-31T23:59:59Z"), Some(-1000));
        assert_eq!(timestamp(b"1970-01-01T00:00:00-00:00"), Some(0));
        assert_eq!(timestamp(b"1970-01-01T23:59:00+23:59"), Some(0));
        assert_eq!(timestamp(b"1969-12-31T00:01:00-23:59"), Some(0));
        assert_eq!(
            timestamp(b"2026-10-03T19:58:10-04:00"),
            timestamp(b"2026-10-03T23:58:10Z")
        );
        assert_eq!(timestamp(b"2000-02-29T00:00:00Z"), Some(951_782_400_000));
        assert_eq!(timestamp(b"2028-02-29T12:34:56Z"), Some(1_835_440_496_000));
        for input in [
            b"1900-02-29T00:00:00Z".as_slice(),
            b"2026-00-01T00:00:00Z",
            b"2026-01-00T00:00:00Z",
            b"2026-04-31T00:00:00Z",
            b"2026-01-01T24:00:00Z",
            b"2026-01-01T00:00:60Z",
            b"2026-01-01T00:00:00+24:00",
            b"2026-01-01T00:00:00-05:60",
            b"2026-01-01t00:00:00Z",
            b"2026-01-01T00:00:00z",
            b"2026-01-01T00:00:00Z\r",
            b"20x6-01-01T00:00:00Z",
            b"2026-01-01T00:00:00.0Z",
        ] {
            assert_eq!(timestamp(input), None, "{input:?}");
        }
    }

    #[test]
    fn decimal_values_are_ascii_only_and_may_have_leading_zeroes() {
        assert_eq!(timestamp(b"000000000000000000000000123"), Some(123));
        assert_eq!(timestamp(b"9223372036854775807"), Some(i64::MAX));
        assert_eq!(timestamp(b"9223372036854775808"), None);
        assert_eq!(price(b"000000000000000000000250"), Some(250));
        assert_eq!(price(b"10000000"), Some(10_000_000));
        assert_eq!(price(b"10000001"), None);
        assert_eq!(price(b"00"), None);
        for input in [
            b"".as_slice(),
            b" 1",
            b"1\r",
            b"1e3",
            b"+5",
            "２５０".as_bytes(),
        ] {
            assert_eq!(timestamp(input), None);
            assert_eq!(price(input), None);
        }
    }
}

#[cfg(test)]
mod indexed_parser_tests {
    use super::*;
    #[test]
    fn indexed_and_scalar_match_at_block_boundaries() {
        const SAMPLES: [&[u8]; 12] = [
            b"a,b,c",
            b"a,\" b\",c",
            b",\"\",",
            b",\"\"\",",
            b"a,b\",c",
            b"a,b\"b,c",
            b"a,\"z,c",
            b"a,b,c,d",
            b"a,,",
            b",,",
            b"\",\",x",
            b"\"\",x,\"y\"",
        ];
        for padding in 0..=135 {
            for sample in SAMPLES {
                let mut line = vec![b'x'; padding];
                line.extend_from_slice(sample);
                assert_eq!(fields::<3>(&line), fields_scalar::<3>(&line), "{line:?}");
                line.reverse();
                assert_eq!(fields::<3>(&line), fields_scalar::<3>(&line), "{line:?}");
            }
        }
    }
    #[test]
    fn ingest_matches_strict_timestamp_across_centuries() {
        for year in [0, 100, 400, 1800, 1900, 1969, 1970, 2000, 2028, 9999] {
            for month in 1..=12 {
                for day in [1, 28, 29, 30, 31] {
                    let f1 = format!("{year:04}-{month:02}-{day:02}T23:59:59Z");
                    if let Some(expected) = timestamp(f1.as_bytes()) {
                        assert_eq!(ingest_timestamp(f1.as_bytes()), Some(expected));
                    }
                }
            }
        }
    }
}
