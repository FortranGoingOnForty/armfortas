//! Reversible source-byte handling for the string-based frontend.

use std::ops::Range;

const ESCAPE: char = '\u{f0000}';
const BYTE_BASE: u32 = 0xf0100;
const BYTE_END: u32 = BYTE_BASE + u8::MAX as u32;

pub(crate) fn to_source_view(bytes: &[u8]) -> String {
    let mut view = String::with_capacity(bytes.len());
    let mut offset = 0;

    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(valid) => {
                push_escaped_utf8(&mut view, valid);
                break;
            }
            Err(error) => {
                let valid_len = error.valid_up_to();
                if valid_len > 0 {
                    let valid = std::str::from_utf8(&bytes[offset..offset + valid_len])
                        .expect("from_utf8 reported an invalid valid prefix");
                    push_escaped_utf8(&mut view, valid);
                    offset += valid_len;
                }

                let invalid_len = error
                    .error_len()
                    .unwrap_or_else(|| bytes.len().saturating_sub(offset));
                for &byte in &bytes[offset..offset + invalid_len] {
                    view.push(ESCAPE);
                    view.push(byte_marker(byte));
                }
                offset += invalid_len;
            }
        }
    }

    view
}

pub(crate) fn escape_utf8(text: &str) -> String {
    let mut view = String::with_capacity(text.len());
    push_escaped_utf8(&mut view, text);
    view
}

pub(crate) fn from_source_view(view: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(view.len());
    let mut chars = view.chars();

    while let Some(ch) = chars.next() {
        if ch == ESCAPE {
            match chars.clone().next() {
                Some(escaped) if escaped == ESCAPE => {
                    chars.next();
                    push_utf8(&mut bytes, ESCAPE);
                }
                Some(escaped) if marker_byte(escaped).is_some() => {
                    chars.next();
                    bytes.push(marker_byte(escaped).expect("checked source-byte marker"));
                }
                _ => push_utf8(&mut bytes, ESCAPE),
            }
        } else {
            push_utf8(&mut bytes, ch);
        }
    }

    bytes
}

pub(crate) fn source_byte_len(view: &str) -> usize {
    source_byte_offset(view, view.len())
}

pub(crate) fn source_byte_offset(view: &str, encoded_offset: usize) -> usize {
    let encoded_offset = encoded_offset.min(view.len());
    let mut source_offset = 0;
    let mut chars = view.char_indices().peekable();

    while let Some((start, ch)) = chars.next() {
        if start >= encoded_offset {
            break;
        }

        if ch == ESCAPE {
            if let Some(&(escaped_start, escaped)) = chars.peek() {
                if escaped == ESCAPE || marker_byte(escaped).is_some() {
                    let escaped_end = escaped_start + escaped.len_utf8();
                    let source_width = if escaped == ESCAPE {
                        ESCAPE.len_utf8()
                    } else {
                        1
                    };
                    if encoded_offset < escaped_end {
                        return source_offset;
                    }
                    source_offset += source_width;
                    chars.next();
                    continue;
                }
            }
        }

        let end = start + ch.len_utf8();
        if encoded_offset < end {
            return source_offset + encoded_offset - start;
        } else {
            source_offset += ch.len_utf8();
        }
    }

    source_offset
}

pub(crate) fn source_byte_range_len(view: &str, range: Range<usize>) -> usize {
    let start = range.start.min(view.len());
    let end = range.end.max(start).min(view.len());
    let mut scan_start = start;
    while scan_start > 0 {
        let Some((previous, ch)) = view[..scan_start].char_indices().next_back() else {
            break;
        };
        if ch != ESCAPE {
            break;
        }
        scan_start = previous;
    }

    let segment = &view[scan_start..];
    source_byte_offset(segment, end - scan_start)
        .saturating_sub(source_byte_offset(segment, start - scan_start))
}

pub(crate) fn display_source_view(view: &str) -> String {
    let mut display = String::with_capacity(view.len());
    let mut chars = view.chars().peekable();
    while let Some((ch, _)) = next_display_unit(&mut chars) {
        display.push(ch);
    }
    display
}

pub(crate) fn display_column(view: &str, source_byte_offset: usize) -> usize {
    let mut chars = view.chars().peekable();
    let mut consumed = 0usize;
    let mut column = 0usize;

    while let Some((_, source_width)) = next_display_unit(&mut chars) {
        if consumed.saturating_add(source_width) > source_byte_offset {
            break;
        }
        consumed += source_width;
        column += 1;
    }

    column
}

pub(crate) fn leading_invalid_byte(view: &str) -> Option<u8> {
    let mut chars = view.chars();
    if chars.next()? != ESCAPE {
        return None;
    }
    marker_byte(chars.next()?)
}

pub(crate) fn leading_display_char(view: &str) -> Option<char> {
    next_display_unit(&mut view.chars().peekable()).map(|(ch, _)| ch)
}

fn push_escaped_utf8(view: &mut String, text: &str) {
    for ch in text.chars() {
        if ch == ESCAPE {
            view.push(ESCAPE);
        }
        view.push(ch);
    }
}

fn byte_marker(byte: u8) -> char {
    char::from_u32(BYTE_BASE + byte as u32).expect("source-byte marker must be a scalar value")
}

fn marker_byte(ch: char) -> Option<u8> {
    let value = ch as u32;
    if (BYTE_BASE..=BYTE_END).contains(&value) {
        Some((value - BYTE_BASE) as u8)
    } else {
        None
    }
}

fn next_display_unit(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<(char, usize)> {
    let ch = chars.next()?;
    if ch == ESCAPE {
        match chars.peek().copied() {
            Some(escaped) if escaped == ESCAPE => {
                chars.next();
                return Some((ESCAPE, ESCAPE.len_utf8()));
            }
            Some(escaped) if marker_byte(escaped).is_some() => {
                chars.next();
                return Some(('\u{fffd}', 1));
            }
            _ => {}
        }
    }
    Some((ch, ch.len_utf8()))
}

fn push_utf8(bytes: &mut Vec<u8>, ch: char) {
    let mut encoded = [0; 4];
    bytes.extend_from_slice(ch.encode_utf8(&mut encoded).as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_bytes_round_trip() {
        let bytes: Vec<u8> = (0..=u8::MAX).collect();
        assert_eq!(from_source_view(&to_source_view(&bytes)), bytes);
    }

    #[test]
    fn reserved_valid_scalars_round_trip() {
        let text = format!("a{ESCAPE}{}z", byte_marker(0xff));
        let view = escape_utf8(&text);
        assert_eq!(from_source_view(&view), text.as_bytes());
        assert_eq!(source_byte_len(&view), text.len());
    }

    #[test]
    fn invalid_bytes_count_as_single_source_bytes() {
        let view = to_source_view(&[b'A', 0xff, b'B']);
        assert_eq!(source_byte_len(&view), 3);
        assert_eq!(source_byte_offset(&view, 1), 1);
        assert_eq!(source_byte_offset(&view, 9), 2);
        assert_eq!(source_byte_offset(&view, view.len()), 3);
        assert_eq!(source_byte_range_len(&view, 1..5), 0);
        assert_eq!(source_byte_range_len(&view, 5..9), 1);
        assert_eq!(display_source_view(&view), "A\u{fffd}B");
        assert_eq!(display_column(&view, 0), 0);
        assert_eq!(display_column(&view, 1), 1);
        assert_eq!(display_column(&view, 2), 2);
        assert_eq!(display_column(&view, 3), 3);
        assert_eq!(leading_invalid_byte(&view[1..]), Some(0xff));
        assert_eq!(leading_display_char(&view[1..]), Some('\u{fffd}'));
    }

    #[test]
    fn display_columns_count_utf8_scalars_not_bytes() {
        let view = to_source_view("A\u{e9}B".as_bytes());
        assert_eq!(display_column(&view, 1), 1);
        assert_eq!(display_column(&view, 2), 1);
        assert_eq!(display_column(&view, 3), 2);
        assert_eq!(display_column(&view, 4), 3);
    }
}
