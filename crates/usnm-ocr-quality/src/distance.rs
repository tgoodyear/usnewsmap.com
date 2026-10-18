//! Levenshtein distance (unit-cost insert, delete, substitute), the same number as gold.py's
//! `edit_distance`, computed with Myers' bit-parallel algorithm in Hyyrö's multi-word form:
//! O(⌈m/64⌉·n) word operations for sequences of length m ≤ n, against gold.py's O(m·n) Python loop.

use std::collections::HashMap;
use std::hash::Hash;

const W: usize = 64;

/// One 64-row block of one column (edlib's `calculateBlock`). `hin` is the horizontal delta
/// entering the block's top row (-1, 0 or +1); returns the delta leaving the row selected by `out_bit`.
#[inline(always)]
fn block(pv: &mut u64, mv: &mut u64, mut eq: u64, hin: i32, out_bit: u64) -> i32 {
    let hin_neg = u64::from(hin < 0);
    let xv = eq | *mv;
    eq |= hin_neg;
    let xh = (((eq & *pv).wrapping_add(*pv)) ^ *pv) | eq;
    let mut ph = *mv | !(xh | *pv);
    let mut mh = *pv & xh;
    let hout = i32::from(ph & out_bit != 0) - i32::from(mh & out_bit != 0);
    ph <<= 1;
    mh <<= 1;
    mh |= hin_neg;
    ph |= u64::from(hin > 0);
    *pv = mh | !(xv | ph);
    *mv = ph & xv;
    hout
}

/// Specialised 64-row block where `out_bit` is `1 << 63` (all non-final blocks).
#[inline(always)]
fn block_msb(pv: &mut u64, mv: &mut u64, mut eq: u64, hin: i32) -> i32 {
    let hin_neg = u64::from(hin < 0);
    let xv = eq | *mv;
    eq |= hin_neg;
    let xh = (((eq & *pv).wrapping_add(*pv)) ^ *pv) | eq;
    let mut ph = *mv | !(xh | *pv);
    let mut mh = *pv & xh;
    let hout = ((ph >> 63) as i32) - ((mh >> 63) as i32);
    ph <<= 1;
    mh <<= 1;
    mh |= hin_neg;
    ph |= u64::from(hin > 0);
    *pv = mh | !(xv | ph);
    *mv = ph & xv;
    hout
}

/// Myers bit-parallel edit distance over dense `u32` symbol IDs in `0..vocab_size`.
/// Unrolls 4 columns of `t` at a time so `pv[k]` and `mv[k]` stay in registers across 4 steps.
pub fn levenshtein_u32_dense(a: &[u32], b: &[u32], vocab_size: usize) -> usize {
    let (p, t) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let m = p.len();
    if m == 0 {
        return t.len();
    }
    if m <= W {
        let mut peq = vec![0u64; vocab_size];
        for (i, &c) in p.iter().enumerate() {
            peq[c as usize] |= 1u64 << i;
        }
        let last_bit = 1u64 << (m - 1);
        let mut pv = !0u64;
        let mut mv = 0u64;
        let mut score = m as i64;
        for &c in t {
            let eq = peq.get(c as usize).copied().unwrap_or(0);
            let h = block(&mut pv, &mut mv, eq, 1, last_bit);
            score += i64::from(h);
        }
        return usize::try_from(score).unwrap_or(0);
    }
    let blocks = m.div_ceil(W);
    let mut peq = vec![0u64; vocab_size * blocks];
    for (i, &c) in p.iter().enumerate() {
        peq[(c as usize) * blocks + (i / W)] |= 1u64 << (i % W);
    }
    let last_bit = 1u64 << ((m - 1) % W);
    let mut pv = vec![!0u64; blocks];
    let mut mv = vec![0u64; blocks];
    let mut score = m as i64;
    let mid_blocks = blocks - 1;
    let (chunks, rem) = t.as_chunks::<4>();
    for &[c0, c1, c2, c3] in chunks {
        let base0 = (c0 as usize) * blocks;
        let base1 = (c1 as usize) * blocks;
        let base2 = (c2 as usize) * blocks;
        let base3 = (c3 as usize) * blocks;
        let (mut h0, mut h1, mut h2, mut h3) = (1i32, 1i32, 1i32, 1i32);
        for k in 0..mid_blocks {
            let mut pvk = pv[k];
            let mut mvk = mv[k];
            h0 = block_msb(&mut pvk, &mut mvk, peq[base0 + k], h0);
            h1 = block_msb(&mut pvk, &mut mvk, peq[base1 + k], h1);
            h2 = block_msb(&mut pvk, &mut mvk, peq[base2 + k], h2);
            h3 = block_msb(&mut pvk, &mut mvk, peq[base3 + k], h3);
            pv[k] = pvk;
            mv[k] = mvk;
        }
        let k = mid_blocks;
        let mut pvk = pv[k];
        let mut mvk = mv[k];
        h0 = block(&mut pvk, &mut mvk, peq[base0 + k], h0, last_bit);
        h1 = block(&mut pvk, &mut mvk, peq[base1 + k], h1, last_bit);
        h2 = block(&mut pvk, &mut mvk, peq[base2 + k], h2, last_bit);
        h3 = block(&mut pvk, &mut mvk, peq[base3 + k], h3, last_bit);
        pv[k] = pvk;
        mv[k] = mvk;
        score += i64::from(h0 + h1 + h2 + h3);
    }
    for &c in rem {
        let base = (c as usize) * blocks;
        let mut h = 1i32;
        for k in 0..mid_blocks {
            h = block_msb(&mut pv[k], &mut mv[k], peq[base + k], h);
        }
        let k = mid_blocks;
        h = block(&mut pv[k], &mut mv[k], peq[base + k], h, last_bit);
        score += i64::from(h);
    }
    usize::try_from(score).unwrap_or(0)
}

/// Myers bit-parallel computation of the edit distance between `a` and every prefix of `b`
/// (both reversed when `rev`), matching `align::last_row` in `O(⌈a.len()/64⌉ · b.len())` time.
pub fn last_row_u32_dense(a: &[u32], b: &[u32], rev: bool, vocab_size: usize) -> Vec<u32> {
    let m = a.len();
    let n = b.len();
    if m == 0 {
        return (0..=n as u32).collect();
    }
    let mut out = Vec::with_capacity(n + 1);
    out.push(m as u32);
    if m <= W {
        let mut peq = vec![0u64; vocab_size];
        for i in 0..m {
            let c = if rev { a[m - 1 - i] } else { a[i] };
            peq[c as usize] |= 1u64 << i;
        }
        let last_bit = 1u64 << (m - 1);
        let mut pv = !0u64;
        let mut mv = 0u64;
        let mut score = m as i32;
        for j in 0..n {
            let c = if rev { b[n - 1 - j] } else { b[j] };
            let eq = peq.get(c as usize).copied().unwrap_or(0);
            score += block(&mut pv, &mut mv, eq, 1, last_bit);
            out.push(score as u32);
        }
        return out;
    }
    let blocks = m.div_ceil(W);
    let mut peq = vec![0u64; vocab_size * blocks];
    for i in 0..m {
        let c = if rev { a[m - 1 - i] } else { a[i] };
        peq[(c as usize) * blocks + (i / W)] |= 1u64 << (i % W);
    }
    let last_bit = 1u64 << ((m - 1) % W);
    let mut pv = vec![!0u64; blocks];
    let mut mv = vec![0u64; blocks];
    let mut score = m as i32;
    let mid_blocks = blocks - 1;
    for j in 0..n {
        let c = if rev { b[n - 1 - j] } else { b[j] };
        let base = (c as usize) * blocks;
        let mut h = 1i32;
        for k in 0..mid_blocks {
            h = block_msb(&mut pv[k], &mut mv[k], peq[base + k], h);
        }
        let k = mid_blocks;
        h = block(&mut pv[k], &mut mv[k], peq[base + k], h, last_bit);
        score += h;
        out.push(score as u32);
    }
    out
}

/// The edit distance between two sequences.
pub fn levenshtein<T: Eq + Hash + Copy>(a: &[T], b: &[T]) -> usize {
    // The shorter one is the pattern (the bit vectors), the longer one is scanned.
    let (p, t) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let m = p.len();
    if m == 0 {
        return t.len();
    }
    let mut ids: HashMap<T, u32> = HashMap::with_capacity(m.min(256));
    let mut p_ids = Vec::with_capacity(m);
    for &c in p {
        let next_id = ids.len() as u32;
        let id = *ids.entry(c).or_insert(next_id);
        p_ids.push(id);
    }
    let unmatched_id = ids.len() as u32;
    let vocab_size = ids.len() + 1;
    let t_ids: Vec<u32> = t
        .iter()
        .map(|c| ids.get(c).copied().unwrap_or(unmatched_id))
        .collect();
    levenshtein_u32_dense(&p_ids, &t_ids, vocab_size)
}

/// The edit distance between two character slices.
pub fn chars_slice(a: &[char], b: &[char]) -> usize {
    let (p, t) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let m = p.len();
    if m == 0 {
        return t.len();
    }
    let mut ascii_map = [u32::MAX; 256];
    let mut non_ascii: HashMap<char, u32> = HashMap::new();
    let mut vocab_size = 0usize;
    let mut p_ids = Vec::with_capacity(m);
    for &c in p {
        let cu = c as u32;
        let id = if cu < 256 {
            let slot = &mut ascii_map[cu as usize];
            if *slot == u32::MAX {
                *slot = vocab_size as u32;
                vocab_size += 1;
            }
            *slot
        } else {
            *non_ascii.entry(c).or_insert_with(|| {
                let id = vocab_size as u32;
                vocab_size += 1;
                id
            })
        };
        p_ids.push(id);
    }
    let unmatched_id = vocab_size as u32;
    vocab_size += 1;
    let t_ids: Vec<u32> = t
        .iter()
        .map(|&c| {
            let cu = c as u32;
            if cu < 256 {
                let id = ascii_map[cu as usize];
                if id == u32::MAX {
                    unmatched_id
                } else {
                    id
                }
            } else {
                non_ascii.get(&c).copied().unwrap_or(unmatched_id)
            }
        })
        .collect();
    levenshtein_u32_dense(&p_ids, &t_ids, vocab_size)
}

/// The edit distance between two strings, over Unicode scalar values (Python's `str` indexing).
pub fn chars(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    chars_slice(&a, &b)
}

/// The edit distance between two word lists; equal words are equal symbols.
pub fn words<'a>(a: &[&'a str], b: &[&'a str]) -> usize {
    let (p, t) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let m = p.len();
    if m == 0 {
        return t.len();
    }
    let mut ids: HashMap<&'a str, u32> = HashMap::with_capacity(m);
    let mut p_ids = Vec::with_capacity(m);
    for &w in p {
        let next_id = ids.len() as u32;
        let id = *ids.entry(w).or_insert(next_id);
        p_ids.push(id);
    }
    let unmatched_id = ids.len() as u32;
    let vocab_size = ids.len() + 1;
    let t_ids: Vec<u32> = t
        .iter()
        .map(|w| ids.get(w).copied().unwrap_or(unmatched_id))
        .collect();
    levenshtein_u32_dense(&p_ids, &t_ids, vocab_size)
}

/// gold.py's quadratic loop, for tests.
#[doc(hidden)]
pub fn reference<T: Eq>(a: &[T], b: &[T]) -> usize {
    let (a, b) = if a.len() < b.len() { (b, a) } else { (a, b) };
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, y) in b.iter().enumerate() {
            cur.push(
                (prev[j + 1] + 1)
                    .min(cur[j] + 1)
                    .min(prev[j] + usize::from(x != y)),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small seeded generator, so the test needs no dependency.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[test]
    fn known_values() {
        assert_eq!(chars("", ""), 0);
        assert_eq!(chars("abc", ""), 3);
        assert_eq!(chars("", "abc"), 3);
        assert_eq!(chars("kitten", "sitting"), 3);
        assert_eq!(chars("flaw", "lawn"), 2);
        assert_eq!(chars("é", "e"), 1);
        assert_eq!(words(&["a", "b", "c"], &["a", "c"]), 1);
    }

    #[test]
    fn matches_the_quadratic_loop() {
        let mut r = Lcg(457);
        for round in 0..3000 {
            // Lengths across the 64-row block edges, alphabets from 2 to 40 symbols.
            let la = r.below(if round % 10 == 0 { 300 } else { 140 }) as usize;
            let lb = r.below(if round % 10 == 0 { 300 } else { 140 }) as usize;
            let k = 2 + r.below(39);
            let a: Vec<u64> = (0..la).map(|_| r.below(k)).collect();
            let mut b: Vec<u64> = if round % 2 == 0 {
                (0..lb).map(|_| r.below(k)).collect()
            } else {
                a.clone()
            };
            if round % 2 == 1 {
                for _ in 0..r.below(20) {
                    let at = r.below(b.len() as u64 + 1) as usize;
                    match r.below(3) {
                        0 => b.insert(at, r.below(k)),
                        1 if at < b.len() => {
                            b.remove(at);
                        }
                        _ if at < b.len() => b[at] = r.below(k),
                        _ => {}
                    }
                }
            }
            assert_eq!(levenshtein(&a, &b), reference(&a, &b), "{a:?} {b:?}");
        }
    }
}
