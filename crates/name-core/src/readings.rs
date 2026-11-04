// SPDX-License-Identifier: Apache-2.0

//! Readings: a name in the scripts a query can be in.
//!
//! A query typed in one script has to match a name written in another:
//! `henshin` has to find 日本語, `にほんご` has to find 日本語, `zhongwen`
//! has to find 中文. A **shadow** is the name's characters transliterated
//! into such a script, with a map back to the source characters so that a
//! match can be reported against the name the user sees.
//!
//! The readings come from the UCD's Unihan data (`readings_table.rs`): a
//! Han character's kana reading and its pinyin reading. The romaji comes
//! from the kana table, because Unihan does not carry one: it gives
//! readings in kana and readings in romaji, but not the mapping between
//! them.
//!
//! A name is read character by character, so the shadow of 日本語 is
//! にほんご / ニホンゴ / nihongo -- one reading per character, not a
//! concatenation of a character's alternatives. A character with several
//! readings (日 is ニチ and ひ) is read by its first one, the on-reading,
//! which is what a compound word uses; the alternative readings are not in
//! the table, and the header of `readings_table.rs` says so.
//!
//! The romaji is the one conversion with rules in it, because a person
//! types the small kana and the long vowel mark the way they sound:
//! きって is "kitte" and しゃ is "sha", not "ki" + "tsu" + "te".

use crate::readings_table::{KANA, KANA_READINGS, KANA_TABLE, PINYIN_READINGS, PINYIN_TABLE};
use alloc::borrow::Cow;
use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;

/// The script a shadow is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Script {
    /// Hepburn romaji: `nihongo`.
    Romaji,
    /// Hiragana: `にほんご`.
    Hiragana,
    /// Katakana: `ニホンゴ`.
    Katakana,
    /// Mandarin pinyin without the tone marks: `zhongwen`.
    Pinyin,
}

/// A name's transliteration into another script.
pub struct Shadow {
    /// The shadow text.
    pub text: Vec<u8>,
    /// For each byte of `text`, the index of the source character it came
    /// from. A reading is longer than the character it came from, so the
    /// map is many-to-one.
    pub origin: Vec<u32>,
}

/// The kana of a name, and the source character each came from.
struct KanaSeq {
    /// Every kana of the name, in order.
    kana: Vec<(char, u32)>,
    /// For each source character, the range of `kana` it owns.
    owned: Vec<(u32, u32)>,
}

impl KanaSeq {
    /// The kana of the name, and who owns them.
    fn of(name: &str) -> KanaSeq {
        let mut kana = Vec::new();
        let mut owned = Vec::new();
        for (i, ch) in name.chars().enumerate() {
            let src = i as u32;
            let start = kana.len() as u32;
            match kana_of(ch) {
                Some(Kana::Char(c)) => kana.push((c, src)),
                Some(Kana::Reading(r)) => kana.extend(r.chars().map(|c| (c, src))),
                None => {}
            }
            owned.push((start, kana.len() as u32));
        }
        KanaSeq { kana, owned }
    }
}

/// The kana a character is read as.
enum Kana {
    /// A kana character is itself.
    Char(char),
    /// A Han character is its kana reading.
    Reading(&'static str),
}

impl Shadow {
    /// The name's shadow in `script`.
    ///
    /// `None` when the name has nothing to transliterate: a name with no
    /// kana and no Han character is its own shadow, and the direct matcher
    /// already covers it.
    pub fn of(name: &[u8], script: Script) -> Option<Shadow> {
        let s = core::str::from_utf8(name).ok()?;
        // The cheap necessary condition: every kana and every Han character
        // is at U+3000 or above, and every codepoint at U+3000 or above is
        // UTF-8 encoded with a lead byte of 0xE3 or more. A name without
        // one has nothing to transliterate, and the scan is a byte loop.
        if !name.iter().any(|&b| b >= 0xE3) {
            return None;
        }
        match script {
            Script::Pinyin => shadow_pinyin(s),
            Script::Hiragana | Script::Katakana => shadow_kana(s, script),
            Script::Romaji => shadow_romaji(s),
        }
    }
}

/// The hiragana or katakana shadow.
fn shadow_kana(s: &str, script: Script) -> Option<Shadow> {
    let seq = KanaSeq::of(s);
    if seq.kana.is_empty() {
        return None;
    }
    let convert = if script == Script::Hiragana { to_hiragana } else { to_katakana };
    let mut text = Vec::with_capacity(s.len());
    let mut origin = Vec::with_capacity(s.len());
    let mut buf = [0u8; 4];
    let mut k = 0;
    for (i, ch) in s.chars().enumerate() {
        let src = i as u32;
        let (start, end) = seq.owned[i];
        if start < end {
            for &(c, _) in &seq.kana[start as usize..end as usize] {
                for b in convert(c).encode_utf8(&mut buf).as_bytes() {
                    text.push(*b);
                    origin.push(src);
                }
            }
            k += 1;
        } else {
            for b in ch.encode_utf8(&mut buf).as_bytes() {
                text.push(*b);
                origin.push(src);
            }
        }
        let _ = k;
    }
    Some(Shadow { text, origin })
}

/// The pinyin shadow.
fn shadow_pinyin(s: &str) -> Option<Shadow> {
    let mut text = Vec::with_capacity(s.len());
    let mut origin = Vec::with_capacity(s.len());
    let mut buf = [0u8; 4];
    let mut any = false;
    for (i, ch) in s.chars().enumerate() {
        let src = i as u32;
        if let Some(id) = pinyin_id(ch as u32) {
            any = true;
            for b in PINYIN_READINGS[id].as_bytes() {
                text.push(*b);
                origin.push(src);
            }
            continue;
        }
        for b in ch.encode_utf8(&mut buf).as_bytes() {
            text.push(*b);
            origin.push(src);
        }
    }
    if !any {
        return None;
    }
    Some(Shadow { text, origin })
}

/// The romaji shadow.
fn shadow_romaji(s: &str) -> Option<Shadow> {
    let seq = KanaSeq::of(s);
    if seq.kana.is_empty() {
        return None;
    }
    // The romaji is converted from the whole kana sequence: the small kana
    // and the long vowel mark depend on their neighbours. The result is a
    // fragment per source character, and the characters that were merged
    // into their neighbour are listed apart, so that they are not emitted
    // a second time.
    let (frags, consumed) = romaji_fragments(&seq);
    let mut text = Vec::with_capacity(s.len());
    let mut origin = Vec::with_capacity(s.len());
    let mut buf = [0u8; 4];
    let mut ci = 0;
    let mut fi = 0;
    for (i, ch) in s.chars().enumerate() {
        let src = i as u32;
        while ci < consumed.len() && consumed[ci] < src {
            ci += 1;
        }
        if ci < consumed.len() && consumed[ci] == src {
            // A small ya/yu/yo that was merged into the kana before it.
            continue;
        }
        while fi < frags.len() && frags[fi].1 < src {
            fi += 1;
        }
        if fi < frags.len() && frags[fi].1 == src {
            while fi < frags.len() && frags[fi].1 == src {
                for b in frags[fi].0.as_bytes() {
                    text.push(*b);
                    origin.push(src);
                }
                fi += 1;
            }
        } else {
            for b in ch.encode_utf8(&mut buf).as_bytes() {
                text.push(*b);
                origin.push(src);
            }
        }
    }
    Some(Shadow { text, origin })
}

/// The romaji of a kana sequence, one fragment per source character, and
/// the characters that were merged into their neighbour.
///
/// The fragments are attributed to the character whose kana produced them.
/// A small ya/yu/yo merges with the kana before it, so its fragment is
/// the previous character's; the small kana's own character is then
/// consumed and contributes nothing of its own.
fn romaji_fragments(seq: &KanaSeq) -> (Vec<(Cow<'static, str>, u32)>, Vec<u32>) {
    let mut out: Vec<(Cow<'static, str>, u32)> = Vec::new();
    let mut consumed: Vec<u32> = Vec::new();
    let mut prev: Option<(&'static str, usize, u32)> = None;
    let mut i = 0;
    while i < seq.kana.len() {
        let (c, src) = seq.kana[i];
        if is_sokuon(c) {
            // The consonant that follows is doubled: the sokuon IS that
            // consonant, so the doubled letter is its contribution.
            let dbl = seq.kana.get(i + 1).and_then(|&(c2, _)| kana_romaji(c2)).and_then(|r| r.chars().find(|&c| !is_vowel(c)));
            if let Some(k) = dbl {
                out.push((Cow::Owned(k.to_string()), src));
            }
            i += 1;
            continue;
        }
        if is_youon(c) {
            let vowel = youon_vowel(c);
            match prev.take() {
                Some((p, at, psrc)) => {
                    if psrc != src {
                        consumed.push(src);
                    }
                    let merged = match p {
                        // The irregular digraphs: `sha`, not `shya`.
                        "shi" => format!("sh{vowel}"),
                        "chi" => format!("ch{vowel}"),
                        "ji" => format!("j{vowel}"),
                        _ => {
                            let c = p.strip_suffix('i').unwrap_or(p);
                            if c.is_empty() { format!("{p}{vowel}") } else { format!("{c}y{vowel}") }
                        }
                    };
                    out.truncate(at);
                    out.push((Cow::Owned(merged), psrc));
                }
                None => out.push((Cow::Owned(vowel.to_string()), src)),
            }
            i += 1;
            continue;
        }
        if is_long(c) {
            // The long vowel mark repeats the vowel of the mora before it:
            // スーパー is "suupaa", the way a romaji IME types it.
            let last = out.last().and_then(|(f, _)| f.chars().last()).filter(|&c| is_vowel(c));
            if let Some(v) = last {
                out.push((Cow::Owned(v.to_string()), src));
            }
            i += 1;
            continue;
        }
        let r = kana_romaji(c).unwrap_or("");
        if !r.is_empty() {
            prev = Some((r, out.len(), src));
            out.push((Cow::Borrowed(r), src));
        }
        i += 1;
    }
    (out, consumed)
}

/// The kana a character is read as: a kana is itself, a Han character is
/// its kana reading.
fn kana_of(ch: char) -> Option<Kana> {
    if is_kana_char(ch) {
        return Some(Kana::Char(ch));
    }
    let id = kana_id(ch as u32)?;
    Some(Kana::Reading(KANA_READINGS[id]))
}

/// Whether a character is a kana at all.
///
/// The small kana and the long vowel mark are kana even though they have
/// no romaji of their own: they are resolved against their neighbours, so
/// they have to be in the sequence.
fn is_kana_char(ch: char) -> bool {
    let cp = ch as u32;
    (0x3041..=0x309F).contains(&cp) || (0x30A0..=0x30FF).contains(&cp)
}

/// The id of a Han character's kana reading, if it has one.
fn kana_id(cp: u32) -> Option<usize> {
    if cp >= crate::readings_table::LAST_CP {
        return None;
    }
    let i = (cp as usize).checked_sub(crate::readings_table::FIRST_CP as usize)?;
    if i >= KANA_TABLE.len() {
        return None;
    }
    let v = KANA_TABLE[i];
    if v == 0 { None } else { Some(v as usize - 1) }
}

/// The id of a Han character's pinyin reading, if it has one.
fn pinyin_id(cp: u32) -> Option<usize> {
    if cp >= crate::readings_table::LAST_CP {
        return None;
    }
    let i = (cp as usize).checked_sub(crate::readings_table::FIRST_CP as usize)?;
    if i >= PINYIN_TABLE.len() {
        return None;
    }
    let v = PINYIN_TABLE[i];
    if v == 0 { None } else { Some(v as usize - 1) }
}

/// The romaji of one kana.
fn kana_romaji(ch: char) -> Option<&'static str> {
    let cp = ch as u32;
    // The table is sorted by the hiragana codepoint, and its katakana is
    // the hiragana shifted by 0x60, so both columns are sorted.
    if let Ok(i) = KANA.binary_search_by(|&(h, _, _)| first_cp(h).cmp(&cp)) {
        return Some(KANA[i].2);
    }
    if let Ok(i) = KANA.binary_search_by(|&(_, k, _)| first_cp(k).cmp(&cp)) {
        return Some(KANA[i].2);
    }
    None
}

/// The codepoint of a one-character string.
fn first_cp(s: &str) -> u32 {
    s.chars().next().map_or(0, |c| c as u32)
}

/// The hiragana of a katakana character, or the character itself.
fn to_hiragana(ch: char) -> char {
    let cp = ch as u32;
    if (0x30A1..=0x30F6).contains(&cp) { char::from_u32(cp - 0x60).unwrap_or(ch) } else { ch }
}

/// The katakana of a hiragana character, or the character itself.
fn to_katakana(ch: char) -> char {
    let cp = ch as u32;
    if (0x3041..=0x3096).contains(&cp) { char::from_u32(cp + 0x60).unwrap_or(ch) } else { ch }
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

fn is_sokuon(ch: char) -> bool {
    ch == 'っ' || ch == 'ッ'
}

fn is_long(ch: char) -> bool {
    ch == 'ー' || ch == '―'
}

fn is_youon(ch: char) -> bool {
    matches!(ch, 'ゃ' | 'ゅ' | 'ょ' | 'ャ' | 'ュ' | 'ョ')
}

fn youon_vowel(ch: char) -> char {
    match ch {
        'ゃ' | 'ャ' => 'a',
        'ゅ' | 'ュ' => 'u',
        _ => 'o',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn romaji(name: &str) -> String {
        let sh = Shadow::of(name.as_bytes(), Script::Romaji).expect("a romaji shadow");
        String::from_utf8(sh.text).expect("the shadow is utf-8")
    }

    fn kana(name: &str, script: Script) -> String {
        let sh = Shadow::of(name.as_bytes(), script).expect("a kana shadow");
        String::from_utf8(sh.text).expect("the shadow is utf-8")
    }

    /// Whether `needle` is a subsequence of `hay`.
    fn is_subsequence(needle: &str, hay: &str) -> bool {
        let mut it = hay.chars();
        needle.chars().all(|w| it.any(|h| h == w))
    }

    #[test]
    fn a_kanji_name_is_read_as_its_kana() {
        // 日本語 is read character by character, so the shadow is the
        // characters' own readings: ニチ ホン ゴ. A compound reading
        // (ニホンゴ) is not in the data -- it belongs to the word, not to
        // the characters -- so the shadow carries the extra チ and the
        // fuzzy matcher skips it. `nihongo` is a subsequence of
        // `nichihongo`, which is what the matcher needs.
        assert_eq!(romaji("日本語"), "nichihongo");
        assert_eq!(romaji("東京"), "toukyou");
        assert_eq!(kana("日本語", Script::Hiragana), "にちほんご");
        assert_eq!(kana("日本語", Script::Katakana), "ニチホンゴ");
        assert!(is_subsequence("nihongo", &romaji("日本語")));
    }

    #[test]
    fn a_kana_name_is_read_as_its_romaji() {
        assert_eq!(romaji("にほんご"), "nihongo");
        assert_eq!(romaji("ニホンゴ"), "nihongo");
        assert_eq!(kana("ニホンゴ", Script::Hiragana), "にほんご");
    }

    #[test]
    fn the_small_kana_are_typed_the_way_they_sound() {
        // きって is "kitte", not "kitsute".
        assert_eq!(romaji("きって"), "kitte");
        // しゃ is "sha", not "shya" and not "shiya".
        assert_eq!(romaji("しゃ"), "sha");
        assert_eq!(romaji("ちゃ"), "cha");
        assert_eq!(romaji("じゃ"), "ja");
        // The rest take the y.
        assert_eq!(romaji("きゃ"), "kya");
        assert_eq!(romaji("りゅ"), "ryu");
    }

    #[test]
    fn the_long_vowel_mark_repeats_the_vowel() {
        // The long vowel mark repeats the vowel of the mora before it, so
        // スーパー is "suupaa" -- the way a romaji IME types it.
        assert_eq!(romaji("スーパー"), "suupaa");
    }

    #[test]
    fn a_name_without_kana_has_no_shadow() {
        assert!(Shadow::of(b"report.pdf", Script::Romaji).is_none());
        assert!(Shadow::of(b"report.pdf", Script::Pinyin).is_none());
    }

    #[test]
    fn the_shadow_maps_back_to_the_source() {
        let sh = Shadow::of("日本語.txt".as_bytes(), Script::Romaji).expect("a shadow");
        assert_eq!(String::from_utf8(sh.text).unwrap(), "nichihongo.txt");
        // Every byte of the romaji points at the character it came from:
        // "nichi" is 日, "hon" is 本, "go" is 語, and the rest is the
        // suffix.
        assert_eq!(&sh.origin[0..5], &[0, 0, 0, 0, 0]);
        assert_eq!(&sh.origin[5..8], &[1, 1, 1]);
        assert_eq!(&sh.origin[8..10], &[2, 2]);
        assert_eq!(sh.origin[10], 3);
        assert_eq!(sh.origin[11], 4);
    }

    #[test]
    fn a_chinese_name_is_read_as_its_pinyin() {
        let sh = Shadow::of("中文".as_bytes(), Script::Pinyin).expect("a pinyin shadow");
        assert_eq!(String::from_utf8(sh.text).unwrap(), "zhongwen");
    }
}
