// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Natural sorting of file names: `page2` < `page10`, case-insensitive.
//!
//! Plain lexicographic sorting puts `10.jpg` before `2.jpg`, which shuffles pages in archives
//! without leading zeros.

use std::cmp::Ordering;

pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a_rest, mut b_rest) = (a, b);
    loop {
        match (a_rest.chars().next(), b_rest.chars().next()) {
            (None, None) => return a.cmp(b), // equal ignoring case → stable tie-break
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (a_num, a_tail) = split_digits(a_rest);
                let (b_num, b_tail) = split_digits(b_rest);
                let (a_trim, b_trim) = (a_num.trim_start_matches('0'), b_num.trim_start_matches('0'));
                let ord = a_trim
                    .len()
                    .cmp(&b_trim.len())
                    .then_with(|| a_trim.cmp(b_trim))
                    .then_with(|| a_num.len().cmp(&b_num.len()));
                if ord != Ordering::Equal {
                    return ord;
                }
                (a_rest, b_rest) = (a_tail, b_tail);
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                (a_rest, b_rest) = (&a_rest[x.len_utf8()..], &b_rest[y.len_utf8()..]);
            }
        }
    }
}

fn split_digits(s: &str) -> (&str, &str) {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    s.split_at(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_naturally() {
        let mut v = vec!["p10.jpg", "p2.jpg", "P1.jpg", "p02b.jpg", "cover.jpg", "ch2/p1", "ch10/p1"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["ch2/p1", "ch10/p1", "cover.jpg", "P1.jpg", "p2.jpg", "p02b.jpg", "p10.jpg"]);
    }
}
