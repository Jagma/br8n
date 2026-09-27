//! The pack's text analyzer: tokenize, stem, drop stopwords — ported from
//! a standalone postings spike.
//!
//! Every posting in `pack.fts` (a later task) is produced by this code, and
//! query-time term lookups run it too. If it ever disagrees with lbug's own
//! FTS analyzer, the pack returns wrong postings with no error — the two
//! writers's outputs never get compared to each other at runtime. The
//! stemmer below is verified bit-exact with lbug's `stem(w,'porter')` over
//! 36,847 vocabulary types in that spike.

use std::collections::HashSet;
use std::sync::LazyLock;

/// Stamped into the manifest. Any change to `STRIP`, the stopword list, or
/// `stem` MUST bump this, because a pack built by one analyzer and queried by
/// another returns wrong postings with no error.
pub const ANALYZER: &str = "porter+simple+default-stop/2";

/// The character class lbug's FTS extension replaces with a space before
/// tokenizing. Recovered from the extension binary
/// (`~/.lbdb/extension/0.19.0/osx_arm64/fts/libfts.lbug_extension`), which
/// carries the default regex as a literal:
///     [0-9!@#$%^&*()_+={}\[\]:;<>,.?~\/\|'"`-]+
/// A backslash is NOT in that class, so it survives inside a token.
///
/// NOTE the digits are absent here, and that is the whole point of `/2`.
/// They used to lead this string, bug-compatibly with lbug's `simple` FTS
/// tokenizer so the pack's postings would match the store's. There is no FTS
/// index any more and `Store::fts_search` errors — `pack.fts` is the only
/// keyword index in the product — so the compatibility target is gone and only
/// the bug remained: `ADR-0004` and `ADR-0001` both indexed as `adr`, and every
/// CVE, version, port, date and `efs=200` collapsed to its letters. Keyword
/// search exists to find rare literal identifiers; it could not see one.
const STRIP: &str = "!@#$%^&*()_+={}[]:;<>,.?~/|'\"`-";

/// lbug's `simple` tokenizer splits on the SPACE character only — not on
/// generic whitespace. Verified directly:
///     tokenize("a b\tc\nd  e",'simple','') -> ["a", "b\tc\nd", "e"]
/// This is the root cause of the `fts_body` workaround recorded in
/// `src/store/schema.rs`: a literal '\n' welds its two neighbours into one
/// unsearchable token. Runs of spaces produce no empty tokens.
/// Public so the analyzer can be checked against lbug's own `stem()` over the
/// real corpus — see the stage 1b plan's Task 7 Step 2. That check is the only
/// one that can catch a divergence, because the postings file and any reference
/// built with THIS analyzer would be wrong identically.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch == ' ' || (ch.is_ascii() && STRIP.contains(ch)) {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            for lc in ch.to_lowercase() {
                cur.push(lc);
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn stopwords() -> HashSet<String> {
    include_str!("stopwords.txt")
        .lines()
        .map(|s| s.to_string())
        .collect()
}

static STOPWORDS: LazyLock<HashSet<String>> = LazyLock::new(stopwords);

/// Tokenize, drop stopwords, then stem — `StopMode::BeforeStem` in the spike's
/// terms, hardcoded rather than a parameter. It is not an arbitrary
/// simplification: it is the mode every measured result in
/// the spike was produced with, and the default in the
/// spike's own `mkpost` and `measure` tools. The other two variants (stem
/// then drop, or drop on either form) exist in the spike because the spike
/// tested them and they lost.
pub fn analyze(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for t in tokenize(text) {
        if STOPWORDS.contains(&t) {
            continue;
        }
        out.push(stem(&t));
    }
    out.retain(|s| !s.is_empty());
    out
}

// ---------------------------------------------------------------------------
// The ORIGINAL Porter stemmer (Porter 1980) — what `stemmer := 'porter'`
// selects in lbug's FTS extension.
//
// NOT Porter2/"english". lbug's stemmer list contains both and they disagree
// on real vocabulary (`generalization` -> `gener` vs `general`, `news` ->
// `new` vs `news`); `rust-stemmers` ships only Porter2, so using it would
// silently shift the term dictionary out from under lbug's index. This is a
// transliteration of Porter's reference C implementation, keeping its
// structure — `k` is the logical last index and the buffer is only ever
// truncated inside `setto`, so `b.len() >= k+1` always holds. Validated
// word-by-word against lbug's own `stem(w,'porter')` over the live corpus
// vocabulary in the postings spike.
// ---------------------------------------------------------------------------

struct S {
    b: Vec<u8>,
    k: usize,
    j: usize,
}

const NONE: usize = usize::MAX;

impl S {
    fn cons(&self, i: usize) -> bool {
        match self.b[i] {
            b'a' | b'e' | b'i' | b'o' | b'u' => false,
            b'y' => i == 0 || !self.cons(i - 1),
            _ => true,
        }
    }

    fn m(&self) -> usize {
        if self.j == NONE {
            return 0;
        }
        let mut n = 0;
        let mut i = 0usize;
        loop {
            if i > self.j {
                return n;
            }
            if !self.cons(i) {
                break;
            }
            i += 1;
        }
        i += 1;
        loop {
            loop {
                if i > self.j {
                    return n;
                }
                if self.cons(i) {
                    break;
                }
                i += 1;
            }
            i += 1;
            n += 1;
            loop {
                if i > self.j {
                    return n;
                }
                if !self.cons(i) {
                    break;
                }
                i += 1;
            }
            i += 1;
        }
    }

    fn vowelinstem(&self) -> bool {
        self.j != NONE && (0..=self.j).any(|i| !self.cons(i))
    }

    /// Snowball's `porter` does NOT use Porter's C `doublec` (any repeated
    /// consonant). It uses an explicit list, which excludes `cc` and `xx`:
    /// measured, `specced -> specc` and `maxxing -> maxx` in lbug, where the
    /// general rule would give `spec` and `max`.
    fn doublec(&self, j: usize) -> bool {
        if j == NONE || j < 1 || self.b[j] != self.b[j - 1] {
            return false;
        }
        matches!(
            self.b[j],
            b'b' | b'd' | b'f' | b'g' | b'm' | b'n' | b'p' | b'r' | b't'
        )
    }

    fn cvc(&self, i: usize) -> bool {
        i != NONE
            && i >= 2
            && self.cons(i)
            && !self.cons(i - 1)
            && self.cons(i - 2)
            && !matches!(self.b[i], b'w' | b'x' | b'y')
    }

    fn ends(&mut self, s: &str) -> bool {
        let s = s.as_bytes();
        let l = s.len();
        if self.k == NONE || l > self.k + 1 {
            return false;
        }
        if &self.b[self.k + 1 - l..=self.k] != s {
            return false;
        }
        self.j = if self.k + 1 == l { NONE } else { self.k - l };
        true
    }

    fn setto(&mut self, s: &str) {
        self.b.truncate(self.j.wrapping_add(1));
        self.b.extend_from_slice(s.as_bytes());
        self.k = self.b.len().wrapping_sub(1);
    }

    fn r(&mut self, s: &str) {
        if self.m() > 0 {
            self.setto(s);
        }
    }

    fn step1ab(&mut self) {
        if self.b[self.k] == b's' {
            if self.ends("sses") {
                self.k -= 2;
            } else if self.ends("ies") {
                self.setto("i");
            } else if self.k == 0 {
                // `s` alone stems to the empty string in lbug; the reference C
                // driver's `k <= 1` guard would have returned it unchanged.
                self.k = NONE;
            } else if self.b[self.k - 1] != b's' {
                self.k -= 1;
            }
        }
        if self.ends("eed") {
            if self.m() > 0 {
                self.k -= 1;
            }
        } else if (self.ends("ed") || self.ends("ing")) && self.vowelinstem() {
            self.k = self.j;
            if self.ends("at") {
                self.setto("ate");
            } else if self.ends("bl") {
                self.setto("ble");
            } else if self.ends("iz") {
                self.setto("ize");
            } else if self.doublec(self.k) {
                if !matches!(self.b[self.k], b'l' | b's' | b'z') {
                    self.k -= 1;
                }
            } else if self.m() == 1 && self.cvc(self.k) {
                self.setto("e");
            }
        }
    }

    fn step1c(&mut self) {
        if self.ends("y") && self.vowelinstem() {
            self.b[self.k] = b'i';
        }
    }

    fn step2(&mut self) {
        if self.k == NONE || self.k < 1 {
            return;
        }
        match self.b[self.k - 1] {
            b'a' => {
                if self.ends("ational") {
                    self.r("ate");
                    return;
                }
                if self.ends("tional") {
                    self.r("tion");
                }
            }
            b'c' => {
                if self.ends("enci") {
                    self.r("ence");
                    return;
                }
                if self.ends("anci") {
                    self.r("ance");
                }
            }
            b'e' => {
                if self.ends("izer") {
                    self.r("ize");
                }
            }
            b'l' => {
                // `abli -> able`, the 1980 paper's rule. Porter's later C
                // relaxed it to `bli -> ble`; snowball's `porter` did not, and
                // the relaxed form wrongly collapses `possibly -> poss`
                // (lbug: `possibli`).
                if self.ends("abli") {
                    self.r("able");
                    return;
                }
                if self.ends("alli") {
                    self.r("al");
                    return;
                }
                if self.ends("entli") {
                    self.r("ent");
                    return;
                }
                if self.ends("eli") {
                    self.r("e");
                    return;
                }
                if self.ends("ousli") {
                    self.r("ous");
                }
            }
            b'o' => {
                if self.ends("ization") {
                    self.r("ize");
                    return;
                }
                if self.ends("ation") {
                    self.r("ate");
                    return;
                }
                if self.ends("ator") {
                    self.r("ate");
                }
            }
            b's' => {
                if self.ends("alism") {
                    self.r("al");
                    return;
                }
                if self.ends("iveness") {
                    self.r("ive");
                    return;
                }
                if self.ends("fulness") {
                    self.r("ful");
                    return;
                }
                if self.ends("ousness") {
                    self.r("ous");
                }
            }
            b't' => {
                if self.ends("aliti") {
                    self.r("al");
                    return;
                }
                if self.ends("iviti") {
                    self.r("ive");
                    return;
                }
                if self.ends("biliti") {
                    self.r("ble");
                }
            }
            _ => {}
        }
    }

    fn step3(&mut self) {
        if self.k == NONE {
            return;
        }
        match self.b[self.k] {
            b'e' => {
                if self.ends("icate") {
                    self.r("ic");
                    return;
                }
                if self.ends("ative") {
                    self.r("");
                    return;
                }
                if self.ends("alize") {
                    self.r("al");
                }
            }
            b'i' => {
                if self.ends("iciti") {
                    self.r("ic");
                }
            }
            b'l' => {
                if self.ends("ical") {
                    self.r("ic");
                    return;
                }
                if self.ends("ful") {
                    self.r("");
                }
            }
            // Not collapsible into a match guard: `ends` takes `&mut self`,
            // and a guard is evaluated while the match's scrutinee place
            // (`self.b[self.k]`) is still considered borrowed.
            #[allow(clippy::collapsible_match)]
            b's' => {
                if self.ends("ness") {
                    self.r("");
                }
            }
            _ => {}
        }
    }

    fn step4(&mut self) {
        if self.k == NONE || self.k < 1 {
            return;
        }
        let matched = match self.b[self.k - 1] {
            b'a' => self.ends("al"),
            b'c' => self.ends("ance") || self.ends("ence"),
            b'e' => self.ends("er"),
            b'i' => self.ends("ic"),
            b'l' => self.ends("able") || self.ends("ible"),
            b'n' => self.ends("ant") || self.ends("ement") || self.ends("ment") || self.ends("ent"),
            b'o' => {
                if self.ends("ion") && self.j != NONE && matches!(self.b[self.j], b's' | b't') {
                    true
                } else {
                    self.ends("ou")
                }
            }
            b's' => self.ends("ism"),
            b't' => self.ends("ate") || self.ends("iti"),
            b'u' => self.ends("ous"),
            b'v' => self.ends("ive"),
            b'z' => self.ends("ize"),
            _ => false,
        };
        if matched && self.m() > 1 {
            self.k = self.j;
        }
    }

    fn step5(&mut self) {
        if self.k == NONE {
            return;
        }
        // `j` is set ONCE here, as in the reference C: the second `m()` below
        // deliberately measures against the pre-decrement `j`.
        self.j = self.k;
        if self.b[self.k] == b'e' {
            let a = self.m();
            if a > 1 || (a == 1 && !self.cvc(self.k.wrapping_sub(1))) {
                self.k = self.k.wrapping_sub(1);
            }
        }
        // Step 5b is specifically about `ll`, so it cannot go through
        // `doublec` now that `doublec` carries snowball's explicit list (which
        // has no `l`): `install -> instal`, `recall -> recal` in lbug.
        if self.k != NONE
            && self.k >= 1
            && self.b[self.k] == b'l'
            && self.b[self.k - 1] == b'l'
            && self.m() > 1
        {
            self.k -= 1;
        }
    }
}

/// Stem `w`, which must already be lowercased.
///
/// Two deliberate departures from Porter's reference C, both forced by
/// measurement against lbug's `stem(w,'porter')` over the live vocabulary:
///   * NO `k <= 1` early return. The C driver skips words of two letters;
///     libstemmer does not, and `js -> j`, `os -> o`, `vs -> v`, `ey -> ei`
///     are real corpus tokens where that guard alone caused a mismatch.
///   * Non-ASCII bytes are stemmed, not passed through. libstemmer runs the
///     ASCII rules over the raw UTF-8 bytes, so every continuation byte simply
///     behaves as a consonant (`information—users -> information—us`).
///
/// Porter's C also carries a `logi -> log` rule in step 2; snowball's `porter`
/// does not, and keeping it mis-stemmed `apologies` and `technologies`.
pub fn stem(w: &str) -> String {
    if w.is_empty() {
        return String::new();
    }
    let b: Vec<u8> = w.as_bytes().to_vec();
    let k = b.len() - 1;
    let mut s = S { b, k, j: 0 };
    s.step1ab();
    s.step1c();
    s.step2();
    s.step3();
    s.step4();
    s.step5();
    if s.k == NONE {
        return String::new();
    }
    String::from_utf8_lossy(&s.b[..=s.k]).into_owned()
}
