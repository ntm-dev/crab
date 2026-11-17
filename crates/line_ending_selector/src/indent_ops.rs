use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuessedIndent {
    pub insert_spaces: bool,
    pub tab_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub text: String,
}

const MAX_DETECT_LINES: usize = 10_000;
const ALLOWED_TAB_SIZE_GUESSES: [usize; 7] = [2, 4, 6, 8, 3, 5, 7];
const MAX_ALLOWED_TAB_SIZE_GUESS: usize = 8;

fn lines_with_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.split('\n').map(move |raw| {
        let start = offset;
        offset += raw.len() + 1;
        (start, raw.strip_suffix('\r').unwrap_or(raw))
    })
}

fn leading_whitespace_len(line: &str) -> usize {
    line.bytes()
        .take_while(|byte| *byte == b' ' || *byte == b'\t')
        .count()
}

fn spaces_diff(
    a: &[u8],
    a_indent: usize,
    b: &[u8],
    b_indent: usize,
) -> (usize, bool) {
    let mut common = 0;
    while common < a_indent && common < b_indent && a[common] == b[common] {
        common += 1;
    }

    let count = |line: &[u8], indent: usize| -> (usize, usize) {
        let mut spaces = 0usize;
        let mut tabs = 0usize;
        for byte in &line[common..indent] {
            if *byte == b' ' {
                spaces += 1;
            } else {
                tabs += 1;
            }
        }
        (spaces, tabs)
    };
    let (a_spaces, a_tabs) = count(a, a_indent);
    let (b_spaces, b_tabs) = count(b, b_indent);

    if (a_spaces > 0 && a_tabs > 0) || (b_spaces > 0 && b_tabs > 0) {
        return (0, false);
    }

    let tabs_diff = a_tabs.abs_diff(b_tabs);
    let spaces_diff = a_spaces.abs_diff(b_spaces);

    if tabs_diff == 0 {
        let mut looks_like_alignment = false;
        if spaces_diff > 0
            && b_spaces >= 1
            && b_spaces - 1 < a.len()
            && b_spaces < b.len()
            && b[b_spaces] != b' '
            && a[b_spaces - 1] == b' '
            && a.last() == Some(&b',')
        {
            looks_like_alignment = true;
        }
        return (spaces_diff, looks_like_alignment);
    }
    if spaces_diff % tabs_diff == 0 {
        return (spaces_diff / tabs_diff, false);
    }
    (0, false)
}

pub fn guess_indentation(
    text: &str,
    default_tab_size: u32,
    default_insert_spaces: bool,
) -> GuessedIndent {
    let mut lines_with_tabs = 0usize;
    let mut lines_with_spaces = 0usize;
    let mut previous_line: &[u8] = b"";
    let mut previous_indent = 0usize;
    let mut diff_counts = [0usize; MAX_ALLOWED_TAB_SIZE_GUESS + 1];

    for (_, line) in lines_with_offsets(text).take(MAX_DETECT_LINES) {
        let bytes = line.as_bytes();
        let mut spaces = 0usize;
        let mut tabs = 0usize;
        let mut indent = None;
        for (index, byte) in bytes.iter().enumerate() {
            match byte {
                b'\t' => tabs += 1,
                b' ' => spaces += 1,
                _ => {
                    indent = Some(index);
                    break;
                }
            }
        }
        let Some(indent) = indent else {
            continue;
        };

        if tabs > 0 {
            lines_with_tabs += 1;
        } else if spaces > 1 {
            lines_with_spaces += 1;
        }

        let (diff, looks_like_alignment) =
            spaces_diff(previous_line, previous_indent, bytes, indent);

        if looks_like_alignment && !(default_insert_spaces && default_tab_size as usize == diff) {
            continue;
        }

        if diff <= MAX_ALLOWED_TAB_SIZE_GUESS {
            diff_counts[diff] += 1;
        }

        previous_line = bytes;
        previous_indent = indent;
    }

    let mut insert_spaces = default_insert_spaces;
    if lines_with_tabs != lines_with_spaces {
        insert_spaces = lines_with_tabs < lines_with_spaces;
    }

    let mut tab_size = default_tab_size;
    if insert_spaces {
        let mut best_score = 0;
        for candidate in ALLOWED_TAB_SIZE_GUESSES {
            let score = diff_counts[candidate];
            if score > best_score {
                best_score = score;
                tab_size = candidate as u32;
            }
        }
        if tab_size == 4
            && diff_counts[4] > 0
            && diff_counts[2] > 0
            && diff_counts[2] * 3 >= diff_counts[4] * 2
        {
            tab_size = 2;
        }
    }

    GuessedIndent {
        insert_spaces,
        tab_size,
    }
}

fn indent_width(prefix: &str, tab_size: usize) -> usize {
    let mut width = 0;
    for byte in prefix.bytes() {
        if byte == b'\t' {
            width += tab_size - width % tab_size;
        } else {
            width += 1;
        }
    }
    width
}

pub fn convert_to_spaces(text: &str, tab_size: u32) -> Vec<TextEdit> {
    let tab_size = tab_size.max(1) as usize;
    let mut edits = Vec::new();
    for (start, line) in lines_with_offsets(text) {
        let prefix_len = leading_whitespace_len(line);
        let prefix = &line[..prefix_len];
        if !prefix.contains('\t') {
            continue;
        }
        edits.push(TextEdit {
            range: start..start + prefix_len,
            text: " ".repeat(indent_width(prefix, tab_size)),
        });
    }
    edits
}

pub fn convert_to_tabs(text: &str, tab_size: u32) -> Vec<TextEdit> {
    let tab_size = tab_size.max(1) as usize;
    let mut edits = Vec::new();
    for (start, line) in lines_with_offsets(text) {
        let prefix_len = leading_whitespace_len(line);
        if prefix_len == 0 || prefix_len == line.len() {
            continue;
        }
        let prefix = &line[..prefix_len];
        let width = indent_width(prefix, tab_size);
        let replacement = format!(
            "{}{}",
            "\t".repeat(width / tab_size),
            " ".repeat(width % tab_size)
        );
        if replacement != prefix {
            edits.push(TextEdit {
                range: start..start + prefix_len,
                text: replacement,
            });
        }
    }
    edits
}

pub fn trim_trailing_whitespace(text: &str) -> Vec<TextEdit> {
    let mut edits = Vec::new();
    for (start, line) in lines_with_offsets(text) {
        let trimmed_len = line.trim_end_matches([' ', '\t']).len();
        if trimmed_len < line.len() {
            edits.push(TextEdit {
                range: start + trimmed_len..start + line.len(),
                text: String::new(),
            });
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, edits: &[TextEdit]) -> String {
        let mut result = text.to_string();
        for edit in edits.iter().rev() {
            result.replace_range(edit.range.clone(), &edit.text);
        }
        result
    }

    #[test]
    fn detects_four_space_json() {
        let text = "{\n    \"a\": {\n        \"b\": 1\n    },\n    \"c\": [\n        1,\n        2\n    ]\n}\n";
        assert_eq!(
            guess_indentation(text, 2, true),
            GuessedIndent {
                insert_spaces: true,
                tab_size: 4
            }
        );
    }

    #[test]
    fn guesses_two_spaces_and_tabs() {
        let two = "a:\n  b:\n    c: 1\n  d: 2\n";
        assert_eq!(
            guess_indentation(two, 4, true),
            GuessedIndent {
                insert_spaces: true,
                tab_size: 2
            }
        );
        let tabs = "fn x() {\n\tlet a = 1;\n\tif a {\n\t\tb();\n\t}\n}\n";
        assert_eq!(
            guess_indentation(tabs, 4, true),
            GuessedIndent {
                insert_spaces: false,
                tab_size: 4
            }
        );
    }

    #[test]
    fn keeps_defaults_without_indentation() {
        let expected = GuessedIndent {
            insert_spaces: true,
            tab_size: 3,
        };
        assert_eq!(guess_indentation("a\nb\n\n", 3, true), expected);
        assert_eq!(guess_indentation("", 3, true), expected);
    }

    #[test]
    fn two_wins_over_four_when_close() {
        let text = "a:\n  b:\n    c:\n      d:\n        e: 1\n  f:\n      g: 1\n";
        let guessed = guess_indentation(text, 4, true);
        assert_eq!(guessed.tab_size, 2);
    }

    #[test]
    fn ignores_alignment_continuation_lines() {
        let text = "const a = b + c,\n      d = b - c;\nfoo();\n    bar();\n";
        let guessed = guess_indentation(text, 2, true);
        assert_eq!(guessed.tab_size, 4);
    }

    #[test]
    fn converts_tabs_to_spaces() {
        let text = "a\n\tb\n\t\tc\n  \td\ne\n";
        let edits = convert_to_spaces(text, 4);
        assert_eq!(apply(text, &edits), "a\n    b\n        c\n    d\ne\n");
    }

    #[test]
    fn converts_spaces_to_tabs() {
        let text = "a\n    b\n      c\n        d\n\ne\n";
        let edits = convert_to_tabs(text, 4);
        assert_eq!(apply(text, &edits), "a\n\tb\n\t  c\n\t\td\n\ne\n");
    }

    #[test]
    fn conversion_keeps_crlf_and_blank_lines() {
        let text = "a\r\n\tb\r\n    \r\n";
        let edits = convert_to_spaces(text, 2);
        assert_eq!(apply(text, &edits), "a\r\n  b\r\n    \r\n");
        let edits = convert_to_tabs("x\r\n    y\r\n    \r\n", 4);
        assert_eq!(apply("x\r\n    y\r\n    \r\n", &edits), "x\r\n\ty\r\n    \r\n");
    }

    #[test]
    fn trims_trailing_whitespace() {
        let text = "a  \nb\t\n  \nc\r\nd \r\n";
        let edits = trim_trailing_whitespace(text);
        assert_eq!(apply(text, &edits), "a\nb\n\nc\r\nd\r\n");
    }
}
