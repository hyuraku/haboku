//! エディタ用の Markdown ハイライタ。syntect（`iced_highlighter`）の置き換え。
//!
//! 置き換えた理由は「配色を幽玄パレットに合わせる」ため（ADR-0009）。
//! `iced_highlighter::Theme` は閉じた enum でカスタム配色を受け付けず、syntect の
//! テーマを自前で組むには依存（`two_face`）と語彙（ScopeSelectors）を丸ごと抱えることになる。
//! このエディタが渡すトークンは常に `"md"` だけなので、全文法エンジンは要らない。
//! frontmatter・見出し・コードの 3 区分が判れば、意匠が求める配色は全部塗れる。
//!
//! 色は決めない。ここは「この範囲は何であるか」（[`Kind`]）だけを返し、
//! 色と書体への変換は UI 側（`main.rs` の `markdown_format`）が持つ。

use std::ops::Range;

use iced::advanced::text::highlighter;

/// 1 行の中のハイライト区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 冒頭の `---` で囲まれた frontmatter（区切り行も含む）。
    Frontmatter,
    /// `#` 見出しの行全体。
    Heading,
    /// フェンスコードブロック（フェンス行も含む）とインラインコード。
    Code,
}

/// 行を跨いで持ち越す状態。**行頭時点**の状態を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineState {
    Normal,
    /// frontmatter の内側。閉じの `---` までこの状態が続く。
    Frontmatter,
    /// フェンスコードブロックの内側。閉じの ``` までこの状態が続く。
    Code,
}

/// 1 行を区分けする純粋関数。戻り値は `(この行のスパン, 次の行頭の状態)`。
///
/// `is_first_line` が要るのは frontmatter のため。`---` が frontmatter を開けるのは
/// **ファイルの 1 行目だけ**で、本文中の `---` は水平線なので状態を変えない。
fn classify_line(
    line: &str,
    state: LineState,
    is_first_line: bool,
) -> (Vec<(Range<usize>, Kind)>, LineState) {
    let whole = || vec![(0..line.len(), Kind::Frontmatter)];

    match state {
        LineState::Frontmatter => {
            let next = if line.trim_end() == "---" {
                LineState::Normal
            } else {
                LineState::Frontmatter
            };
            (whole(), next)
        }
        LineState::Code => {
            let next = if line.trim_start().starts_with("```") {
                LineState::Normal
            } else {
                LineState::Code
            };
            (vec![(0..line.len(), Kind::Code)], next)
        }
        LineState::Normal => {
            if is_first_line && line.trim_end() == "---" {
                return (whole(), LineState::Frontmatter);
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") {
                return (vec![(0..line.len(), Kind::Code)], LineState::Code);
            }
            if is_heading(trimmed) {
                return (vec![(0..line.len(), Kind::Heading)], LineState::Normal);
            }
            (inline_code_spans(line), LineState::Normal)
        }
    }
}

/// `#`〜`######` + 空白（または行末）で始まる行を見出しとみなす。
fn is_heading(trimmed: &str) -> bool {
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&hashes)
        && trimmed[hashes..]
            .bytes()
            .next()
            .is_none_or(|b| b == b' ' || b == b'\t')
}

/// バッククォートの対で囲まれたインラインコード（両端の ` を含む）を拾う。
/// 閉じられていない ` は無視する。範囲は**バイト単位**（` は ASCII なので
/// 文字境界を跨がない。日本語の本文が間にあっても安全）。
fn inline_code_spans(line: &str) -> Vec<(Range<usize>, Kind)> {
    let mut spans = Vec::new();
    let mut open: Option<usize> = None;
    for (i, b) in line.bytes().enumerate() {
        if b == b'`' {
            match open.take() {
                None => open = Some(i),
                Some(start) => spans.push((start..i + 1, Kind::Code)),
            }
        }
    }
    spans
}

/// [`highlighter::Highlighter`] の実装。
///
/// iced の増分プロトコルは「`change_line(n)` の後、n 行目から順に `highlight_line` が
/// 呼び直される」。任意の n から再開するには **各行頭の状態**が要るので、
/// `states[i] = i 行目の行頭の状態` を丸ごと持つ（1 行 1 バイトの enum。数千行でも数 KB）。
#[derive(Debug)]
pub struct MarkdownHighlighter {
    states: Vec<LineState>,
    current_line: usize,
}

impl highlighter::Highlighter for MarkdownHighlighter {
    type Settings = ();
    type Highlight = Kind;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Kind)>;

    fn new(_settings: &Self::Settings) -> Self {
        MarkdownHighlighter {
            states: vec![LineState::Normal],
            current_line: 0,
        }
    }

    fn update(&mut self, _new_settings: &Self::Settings) {
        self.change_line(0);
    }

    fn change_line(&mut self, line: usize) {
        if line < self.states.len() {
            self.states.truncate(line + 1);
            self.current_line = line;
        } else {
            // 知らない行まで飛ばされたら先頭からやり直す（iced_highlighter と同じ扱い）。
            self.states.truncate(1);
            self.current_line = 0;
        }
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let state = *self.states.last().expect("states must not be empty");
        let (spans, next) = classify_line(line, state, self.current_line == 0);
        self.current_line += 1;
        self.states.push(next);
        spans
            .into_iter()
            .filter(|(range, _)| !range.is_empty())
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn current_line(&self) -> usize {
        self.current_line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::text::highlighter::Highlighter as _;

    /// 行の並びを順に流して、各行のスパンを集める。
    fn run(lines: &[&str]) -> Vec<Vec<(Range<usize>, Kind)>> {
        let mut hl = MarkdownHighlighter::new(&());
        lines
            .iter()
            .map(|line| hl.highlight_line(line).collect())
            .collect()
    }

    #[test]
    fn frontmatter_opens_only_at_first_line() {
        let spans = run(&["---", "title: \"墨\"", "---", "本文"]);
        assert_eq!(spans[0], vec![(0..3, Kind::Frontmatter)]);
        assert_eq!(spans[1], vec![(0..12, Kind::Frontmatter)]); // 日本語込みのバイト長
        assert_eq!(spans[2], vec![(0..3, Kind::Frontmatter)]);
        assert_eq!(spans[3], vec![]); // 閉じたあとは平文
    }

    #[test]
    fn dash_rule_mid_document_is_not_frontmatter() {
        let spans = run(&["本文", "---", "続き"]);
        assert_eq!(spans[1], vec![]); // 水平線。frontmatter 扱いにしない
        assert_eq!(spans[2], vec![]);
    }

    #[test]
    fn fenced_code_block_spans_until_closing_fence() {
        let spans = run(&["```rust", "let x = 1;", "```", "後段"]);
        assert_eq!(spans[0], vec![(0..7, Kind::Code)]);
        assert_eq!(spans[1], vec![(0..10, Kind::Code)]);
        assert_eq!(spans[2], vec![(0..3, Kind::Code)]);
        assert_eq!(spans[3], vec![]);
    }

    #[test]
    fn heading_inside_code_block_is_code() {
        let spans = run(&["```", "# コメントであって見出しではない", "```"]);
        assert_eq!(spans[1][0].1, Kind::Code);
    }

    #[test]
    fn headings_h1_to_h6_but_not_hash_without_space() {
        for h in ["# 見出し", "###### 見出し", "#", "##\tタブ"] {
            let spans = run(&[h]);
            assert_eq!(spans[0], vec![(0..h.len(), Kind::Heading)], "{h}");
        }
        assert_eq!(run(&["#タグではない見出し記法"])[0], vec![]);
        assert_eq!(run(&["####### 7個は見出しではない"])[0], vec![]);
    }

    #[test]
    fn inline_code_pairs_and_unclosed_backtick() {
        let line = "値は `let 値 = 1` と `x` で、閉じない ` は無視";
        let spans = run(&[line]);
        assert_eq!(spans[0].len(), 2);
        for (range, kind) in &spans[0] {
            assert_eq!(*kind, Kind::Code);
            assert!(line.get(range.clone()).is_some_and(|s| s.starts_with('`')));
            assert!(line[range.clone()].ends_with('`'));
        }
    }

    #[test]
    fn empty_line_emits_no_spans() {
        assert_eq!(run(&["", "# x", ""]), vec![
            vec![],
            vec![(0..3, Kind::Heading)],
            vec![],
        ]);
    }

    #[test]
    fn change_line_replays_state_from_that_line() {
        let mut hl = MarkdownHighlighter::new(&());
        for line in ["```", "中身", "```"] {
            let _ = hl.highlight_line(line).count();
        }
        // 1 行目（開きフェンスの次）を書き換えたと通知 → 1 行目の行頭は Code のまま。
        hl.change_line(1);
        assert_eq!(hl.current_line(), 1);
        let spans: Vec<_> = hl.highlight_line("書き換えた中身").collect();
        assert_eq!(spans[0].1, Kind::Code);

        // 0 行目（開きフェンス自体）を消したと通知 → 平文に戻る。
        hl.change_line(0);
        let spans: Vec<_> = hl.highlight_line("もうフェンスではない").collect();
        assert_eq!(spans, vec![]);
    }

    #[test]
    fn change_line_beyond_known_lines_restarts() {
        let mut hl = MarkdownHighlighter::new(&());
        let _ = hl.highlight_line("# a").count();
        hl.change_line(99);
        assert_eq!(hl.current_line(), 0);
        // 先頭からなので frontmatter がまた開ける。
        let spans: Vec<_> = hl.highlight_line("---").collect();
        assert_eq!(spans[0].1, Kind::Frontmatter);
    }
}
