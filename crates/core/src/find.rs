//! Text search for the response panel.

#[derive(Debug, PartialEq)]
pub struct Found {
    /// Byte offsets of the match in the searched text (what Slint's text widgets use).
    pub start: usize,
    pub end: usize,
    /// Position of this match among all of them, and how many there are.
    pub index: usize,
    pub total: usize,
}

/// ASCII case-insensitive, so lower-casing never moves a byte offset. Forward gives the first match at or after `from`,
/// backward the last one before it; both wrap around. `None` for an empty needle or no match.
pub fn find(hay: &str, needle: &str, from: usize, backward: bool) -> Option<Found> {
    if needle.is_empty() {
        return None;
    }
    let starts: Vec<usize> = hay
        .to_ascii_lowercase()
        .match_indices(&needle.to_ascii_lowercase())
        .map(|(i, _)| i)
        .collect();
    let index = if backward {
        starts.iter().rposition(|&s| s < from).or(starts.len().checked_sub(1))?
    } else {
        starts
            .iter()
            .position(|&s| s >= from)
            .or((!starts.is_empty()).then_some(0))?
    };
    let start = starts[index];
    Some(Found {
        start,
        end: start + needle.len(),
        index,
        total: starts.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hay: &str, needle: &str, from: usize, back: bool) -> Option<(usize, usize, usize)> {
        find(hay, needle, from, back).map(|f| (f.start, f.index, f.total))
    }

    #[test]
    fn forward_backward_and_wrap() {
        let hay = "ab AB ab";
        assert_eq!(at(hay, "ab", 0, false), Some((0, 0, 3)));
        assert_eq!(at(hay, "ab", 1, false), Some((3, 1, 3)));
        assert_eq!(at(hay, "ab", 4, false), Some((6, 2, 3)));
        assert_eq!(at(hay, "ab", 7, false), Some((0, 0, 3)), "wraps to the first");
        assert_eq!(at(hay, "ab", 6, true), Some((3, 1, 3)));
        assert_eq!(at(hay, "ab", 0, true), Some((6, 2, 3)), "wraps to the last");
    }

    #[test]
    fn offsets_are_bytes_after_multibyte_text() {
        let f = find("é→ name", "NAME", 0, false).unwrap();
        assert_eq!((f.start, f.end), (6, 10));
        assert_eq!(&"é→ name"[f.start..f.end], "name");
    }

    #[test]
    fn nothing_to_find() {
        assert_eq!(find("abc", "", 0, false), None);
        assert_eq!(find("abc", "x", 0, false), None);
        assert_eq!(find("", "x", 0, true), None);
    }
}
