//! あいまい一致。`Cmd+P` の心臓部。
//!
//! 「文字が飛び飛びでも当たる」＝ クエリが対象の**部分列**であればマッチ、という定義。
//! `bstnt` が `Boostnote 終了の件` に当たるのはこの性質による。
//!
//! 総当りの DP なら最適なマッチ位置を選べるが、貪欲法で十分実用になる。
//! 約 1200 件 × 数十文字 × クエリ数文字を毎打鍵で回すので、速いほうを採る。

use std::ops::Range;

#[derive(Debug, Clone)]
pub struct Match {
    /// 大きいほど良い。並び替えにのみ使い、絶対値に意味は無い。
    pub score: i32,
    /// マッチした文字の**バイト範囲**。そのままハイライトに渡せる。
    pub ranges: Vec<Range<usize>>,
}

/// 先頭一致のご褒美。「打ち始めた文字で始まるもの」を最優先したい。
const BONUS_FIRST: i32 = 16;
/// 単語の頭に当たったご褒美。`fx` が `foo-xyz` に当たる類。
const BONUS_WORD_START: i32 = 10;
/// 連続して当たったご褒美。ばらけたマッチより固まったマッチを上に出す。
const BONUS_CONSECUTIVE: i32 = 8;
/// 読み飛ばした分の減点。離れたマッチを下げる。累積しすぎないよう頭打ちにする。
const PENALTY_GAP: i32 = 1;
const PENALTY_GAP_MAX: i32 = 12;

/// `query` が `target` の部分列なら `Some`。大文字小文字は無視する。
///
/// 空クエリは「全部当たる」= スコア 0 で返す。呼び出し側で分岐せずに済む。
pub fn match_query(query: &str, target: &str) -> Option<Match> {
    if query.is_empty() {
        return Some(Match {
            score: 0,
            ranges: Vec::new(),
        });
    }

    // 対象を (元文字列でのバイト範囲, 正規化後の文字) で持つ。
    // 半角カナの濁点合成（ｶ+ﾞ → が）で 2 文字が 1 文字に畳まれることがあるため、
    // 位置は開始バイトではなく範囲で持つ。ハイライトにそのまま渡せる。
    let chars = fold(target);

    let mut score = 0;
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut ti = 0usize; // target 側の走査位置
    let mut last_matched: Option<usize> = None;

    for (_, qc) in fold(query) {
        // qc に当たる最初の位置まで進める
        let found = chars[ti..]
            .iter()
            .position(|(_, tc)| *tc == qc)
            .map(|off| ti + off)?; // 見つからなければ即 None（部分列でない）

        // ── 加点 ──
        if found == 0 {
            score += BONUS_FIRST;
        } else if is_word_boundary(&chars, found) {
            score += BONUS_WORD_START;
        }
        if last_matched == Some(found.wrapping_sub(1)) {
            score += BONUS_CONSECUTIVE;
        }

        // ── 減点：読み飛ばした距離 ──
        let skipped = match last_matched {
            Some(prev) => found.saturating_sub(prev + 1),
            None => found,
        } as i32;
        score -= (skipped * PENALTY_GAP).min(PENALTY_GAP_MAX);

        ranges.push(chars[found].0.clone());
        last_matched = Some(found);
        ti = found + 1;
    }

    // 短い対象を優先する。同じだけ当たったなら、余計な文字が少ないほうが狙いに近い。
    score -= (chars.len() as i32) / 8;

    Some(Match { score, ranges })
}

/// `query` が `target` に**連続して**現れる最初の位置（`target` 基準のバイト範囲）。
///
/// **`match_query` と違って飛び飛びは当たらない。** 本文は長いので、部分列一致だと
/// 任意のクエリがほぼ必ず当たってフィルタにならない（21 万文字のノートは
/// ほぼ全ての文字を含む）。「打った語がそのまま本文にある」という素直な意味にする。
///
/// 正規化の規則は `match_query` と共有する（どちらも `folded()` を通る）。
///
/// **空クエリは `None`。** `match_query` の「空は全部当たる」とはわざと非対称にした。
/// こちらは「探した結果ここに当たった」を返す道具で、位置の無い当たりを表せない。
/// 全件を出すのは呼び出し側の仕事。
pub fn contains_query(query: &str, target: &str) -> Option<Range<usize>> {
    let needle: Vec<char> = folded(query).map(|(_, c)| c).collect();
    if needle.is_empty() {
        return None;
    }

    // 開始位置を 1 単位ずつずらして、そこから連続で一致するかを見る。
    // **再スライスの開始は常に畳んだ単位の境界**なので、濁点の合成ペア（ｶ+ﾞ）を割らない。
    let mut cursor = 0usize;
    while cursor < target.len() {
        let mut rest = folded(&target[cursor..]);
        let Some((head, first)) = rest.next() else {
            break;
        };
        if first == needle[0] {
            let mut matched = 1;
            let mut end = cursor + head.end;
            while matched < needle.len() {
                match rest.next() {
                    Some((range, c)) if c == needle[matched] => {
                        matched += 1;
                        end = cursor + range.end;
                    }
                    _ => break,
                }
            }
            if matched == needle.len() {
                return Some(cursor..end);
            }
        }
        cursor += head.end;
    }
    None
}

/// 文字列を (バイト範囲, 正規化済み文字) の列として**その場で**返す。
///
/// 半角カナの濁点・半濁点（ﾞ ﾟ）は直前の文字と合成する（ｶ+ﾞ → が）。
/// 合成できたときは範囲が元の 2 文字分に広がるので、ハイライトも自然に繋がる。
///
/// **正規化の規則はここが唯一の実装。** `fold()` はこれを集めただけで、
/// 本文検索（`contains_query`）はこれを直接回して確保をゼロにする。
/// 規則が 2 か所に分かれると、タイトルと本文で当たり方がずれる。
fn folded(s: &str) -> Folded<'_> {
    Folded {
        inner: s.char_indices().peekable(),
    }
}

struct Folded<'a> {
    inner: std::iter::Peekable<std::str::CharIndices<'a>>,
}

impl Iterator for Folded<'_> {
    type Item = (Range<usize>, char);

    fn next(&mut self) -> Option<Self::Item> {
        let (i, c) = self.inner.next()?;
        let mut range = i..i + c.len_utf8();
        let mut ch = lower(c);
        // 濁点は**次に**来るので先読みする。畳めたぶん範囲の end だけ伸ばす。
        if let Some(&(j, mark)) = self.inner.peek()
            && matches!(mark, '\u{FF9E}' | '\u{FF9F}')
            && let Some(voiced) = voice(ch, mark == '\u{FF9F}')
        {
            range.end = j + mark.len_utf8();
            ch = voiced;
            self.inner.next();
        }
        Some((range, ch))
    }
}

/// `folded()` を集めたもの。位置で引きたい `match_query` はこちらを使う。
fn fold(s: &str) -> Vec<(Range<usize>, char)> {
    folded(s).collect()
}

/// ひらがな1文字に濁点（semi=false）/ 半濁点（semi=true）を付ける。付かない文字は None。
/// ひらがなブロックは清音の直後に濁音（は→ば）、その次に半濁音（ぱ）が並ぶ。
fn voice(c: char, semi: bool) -> Option<char> {
    if semi {
        return matches!(c, 'は' | 'ひ' | 'ふ' | 'へ' | 'ほ')
            .then(|| char::from_u32(c as u32 + 2))
            .flatten();
    }
    match c {
        'か' | 'き' | 'く' | 'け' | 'こ' | 'さ' | 'し' | 'す' | 'せ' | 'そ' | 'た' | 'ち'
        | 'つ' | 'て' | 'と' | 'は' | 'ひ' | 'ふ' | 'へ' | 'ほ' => {
            char::from_u32(c as u32 + 1)
        }
        'う' => Some('ゔ'),
        _ => None,
    }
}

/// 半角カナ（U+FF66..=U+FF9D）→ ひらがな。濁点合成は fold 側でやる。
const HALFWIDTH_KANA: [char; 56] = [
    'を', 'ぁ', 'ぃ', 'ぅ', 'ぇ', 'ぉ', 'ゃ', 'ゅ', 'ょ', 'っ', 'ー', 'あ', 'い', 'う', 'え', 'お',
    'か', 'き', 'く', 'け', 'こ', 'さ', 'し', 'す', 'せ', 'そ', 'た', 'ち', 'つ', 'て', 'と', 'な',
    'に', 'ぬ', 'ね', 'の', 'は', 'ひ', 'ふ', 'へ', 'ほ', 'ま', 'み', 'む', 'め', 'も', 'や', 'ゆ',
    'よ', 'ら', 'り', 'る', 'れ', 'ろ', 'わ', 'ん',
];

/// 比較用に1文字を均す。**全角→半角に加えて、かなも一つの表記に寄せる。**
///
/// 日本語入力モードのまま数字や英字を打つと全角（`６０` `Ａ`）になり、
/// タイトル側はほぼ半角なので素朴に比較すると一件も当たらない
/// （実際「1 や 60 に反応しない」という形で表面化した）。
/// カタカナ⇄ひらがな・半角カナも、IME の状態や書いた時の気分で揺れるだけで
/// 検索する人にとっては同じ文字。全部ひらがなに畳んで比較する。
fn lower(c: char) -> char {
    let c = match c {
        // 全角 ASCII（！..～ = U+FF01..U+FF5E）は 0xFEE0 引くと半角 ASCII になる。
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        // 全角スペースも半角に寄せる。
        '\u{3000}' => ' ',
        // カタカナ（ァ..ヶ）→ ひらがな。0x60 引くだけで対応が取れる。
        '\u{30A1}'..='\u{30F6}' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
        // 半角カナ → ひらがな。
        '\u{FF66}'..='\u{FF9D}' => HALFWIDTH_KANA[(c as u32 - 0xFF66) as usize],
        other => other,
    };
    c.to_lowercase().next().unwrap_or(c)
}

/// 単語の先頭か。**区切り文字の直後だけ**を見る。
/// 日本語には単語境界がほぼ無いので、実質的には連続ボーナスが効く。
///
/// camelCase の切れ目（`prev.is_lowercase() && cur.is_uppercase()`）も見ていたが、
/// **原理的に到達しないので落とした。** ここへ渡る `chars` は `fold()` が `lower()` を
/// 通したあとの文字で、大文字は残っていない。判定に要る情報を、判定の前に潰していた。
fn is_word_boundary(chars: &[(Range<usize>, char)], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = chars[i - 1].1;
    matches!(prev, ' ' | '-' | '_' | '/' | '.' | '(' | '[' | '、' | '・')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_matches() {
        assert!(match_query("bstnt", "Boostnote 終了の件").is_some());
        assert!(match_query("xyz", "Boostnote").is_none());
    }

    #[test]
    fn japanese_matches() {
        assert!(match_query("移行メモ", "ブーストノート移行メモ").is_some());
    }

    /// 日本語入力モードのまま打つと全角になる。ここが当たらないと
    /// 「数字で検索できない」という形で壊れる（実際に踏んだ）。
    #[test]
    fn fullwidth_query_matches_halfwidth_target() {
        assert!(match_query("６０", "60分で学ぶ最新Webフロントエンド").is_some());
        assert!(match_query("１", "1 Billion Row Challenge (1BRC)").is_some());
        assert!(match_query("ＬＬＭ", "1-bit LLM / Ternary LLM").is_some());
    }

    /// 逆向き（半角クエリ・全角タイトル）も当たること。
    #[test]
    fn halfwidth_query_matches_fullwidth_target() {
        assert!(match_query("60", "６０分で学ぶ").is_some());
    }

    /// カタカナ⇄ひらがな・半角カナは同じ文字として当たること。
    /// 濁点付き半角カナ（ﾎﾞ = 2文字）も合成して1文字（ぼ）として扱う。
    #[test]
    fn kana_variants_match() {
        assert!(match_query("ぶーすと", "Boostnote ブースト移行").is_some());
        assert!(match_query("ブースト", "ぶーすとのメモ").is_some());
        assert!(match_query("ﾎﾞｰﾄ", "ボート競技").is_some());
        assert!(match_query("ぼーと", "ﾎﾞｰﾄの写真").is_some());
        assert!(match_query("ぱん", "ﾊﾟﾝの店").is_some());
    }

    /// **部分列は本文の一致にしない。** この設計の核。
    ///
    /// タイトルは飛び飛びで当ててよいが、本文で同じことをすると
    /// 21 万文字のノートがほぼ全てのクエリに当たり、フィルタとして機能しない。
    /// 方式を取り違えたら必ずここが落ちる。
    #[test]
    fn a_subsequence_is_not_a_body_match() {
        assert!(match_query("bstnt", "Boostnote 終了の件").is_some());
        assert!(contains_query("bstnt", "Boostnote 終了の件").is_none());
    }

    /// 連続一致は最初の出現を返し、その範囲が元の文字列を正しく切り出すこと。
    #[test]
    fn contains_query_returns_the_first_occurrence() {
        let target = "iced の text_editor と iced の scrollable";
        let range = contains_query("iced", target).unwrap();
        assert_eq!(range, 0..4);
        assert_eq!(target.get(range), Some("iced"));

        let target = "前置き。ここに iced がある";
        let range = contains_query("iced", target).unwrap();
        assert_eq!(target.get(range.clone()), Some("iced"));
        assert!(range.start > 0, "先頭以外の位置を返せていない");
    }

    /// 正規化の規則がタイトル側と同じであること。
    #[test]
    fn body_matching_normalizes_like_titles() {
        // 全角クエリ → 半角本文、およびその逆。
        assert!(contains_query("６０", "60 分で学ぶ").is_some());
        assert!(contains_query("60", "６０分で学ぶ").is_some());
        // カタカナ ⇄ ひらがな。
        assert!(contains_query("ブースト", "ぶーすとの記録").is_some());
        // 半角カナの濁点は 2 文字を 1 文字に畳み、範囲は 2 文字ぶんを覆う。
        let range = contains_query("ぼ", "ﾎﾞｰﾄ").unwrap();
        assert_eq!(range, 0..6);
    }

    /// 空クエリは当たらないこと（`match_query` とわざと非対称）。
    #[test]
    fn an_empty_query_does_not_match_a_body() {
        assert!(contains_query("", "なんでも書いてある本文").is_none());
        // タイトル側は逆に「全部当たる」。この非対称は意図したもの。
        assert!(match_query("", "なんでも書いてある本文").is_some());
    }

    /// 畳み込みの結果を (バイト範囲, 正規化後の文字) の列として固定する。
    ///
    /// **正規化の規則はここが唯一の実装で、タイトル検索も本文検索もここを通る。**
    /// リファクタで静かにずれると、両方の当たり方が同時に変わる。
    #[test]
    fn folding_keeps_byte_ranges_aligned() {
        let got: Vec<_> = folded("ﾊﾟnＡあ").collect();
        assert_eq!(
            got,
            vec![
                (0..6, 'ぱ'),  // ﾊ(3) + ﾟ(3) を 1 文字に畳み、範囲は 2 文字ぶん
                (6..7, 'n'),
                (7..10, 'a'), // 全角 Ａ → 半角 a
                (10..13, 'あ'),
            ]
        );
    }

    /// 濁点合成のハイライト範囲は半角カナ2文字分をひとつながりで覆うこと。
    #[test]
    fn halfwidth_dakuten_highlight_spans_both_chars() {
        let m = match_query("ぼ", "ﾎﾞｰﾄ").unwrap();
        // ﾎ(3byte) + ﾞ(3byte) = 0..6
        assert_eq!(m.ranges, vec![0..6]);
    }

    #[test]
    fn prefix_scores_higher_than_scattered() {
        let prefix = match_query("boo", "Boostnote").unwrap();
        let scattered = match_query("boo", "abcbdoeo").unwrap();
        assert!(prefix.score > scattered.score);
    }
}
