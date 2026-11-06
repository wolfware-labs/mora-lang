//! Line diffs for the problems of snapshot and output comparisons.

/// A line diff from `old` to `new`: unchanged lines start with two spaces,
/// removed ones with `- ` and added ones with `+ `. A side that does not end
/// with a newline gets a `\ ` line saying so, as `git diff` does, so a
/// difference in the final newline alone stays visible. Test outputs are short, so
/// the quadratic longest-common-subsequence table is cheap.
pub fn diff(old_text: &str, new_text: &str) -> String {
    let old: Vec<&str> = old_text.lines().collect();
    let new: Vec<&str> = new_text.lines().collect();
    // common[i][j]: length of the longest common subsequence of old[i..] and new[j..].
    let mut common = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            common[i][j] = if old[i] == new[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let mut out = String::new();
    let mut line = |prefix: &str, text: &str| {
        out.push_str(prefix);
        out.push_str(text);
        out.push('\n');
    };
    let (mut i, mut j) = (0, 0);
    while i < old.len() || j < new.len() {
        if i < old.len() && j < new.len() && old[i] == new[j] {
            line("  ", old[i]);
            i += 1;
            j += 1;
        } else if j == new.len() || (i < old.len() && common[i + 1][j] >= common[i][j + 1]) {
            line("- ", old[i]);
            i += 1;
        } else {
            line("+ ", new[j]);
            j += 1;
        }
    }
    for (prefix, text) in [("-", old_text), ("+", new_text)] {
        if !text.is_empty() && !text.ends_with('\n') {
            line(
                "\\ ",
                &format!("no newline at the end of the {prefix} side"),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_removed_and_added_lines() {
        assert_eq!(
            diff("a\nb\nc\n", "a\nx\nc\nd\n"),
            "  a\n- b\n+ x\n  c\n+ d\n"
        );
    }

    #[test]
    fn shows_a_missing_final_newline() {
        assert_eq!(
            diff("one\n", "one"),
            "  one\n\\ no newline at the end of the + side\n"
        );
    }

    #[test]
    fn handles_empty_sides() {
        assert_eq!(diff("", "a\n"), "+ a\n");
        assert_eq!(diff("a\n", ""), "- a\n");
        assert_eq!(diff("", ""), "");
    }

    #[test]
    fn keeps_the_longest_run_of_common_lines() {
        assert_eq!(diff("a\n", "b\n"), "- a\n+ b\n");
        assert_eq!(diff("a\na\n", "b\na\n"), "- a\n+ b\n  a\n");
        assert_eq!(diff("a\na\n", "b\na\na\n"), "+ b\n  a\n  a\n");
        assert_eq!(diff("a\nb\na\n", "c\na\n"), "- a\n- b\n+ c\n  a\n");
        assert_eq!(
            diff("x\na\nb\nc\n", "a\nb\nc\ny\n"),
            "- x\n  a\n  b\n  c\n+ y\n"
        );
        assert_eq!(
            diff("a\nb\nc\nd\n", "b\nc\na\nd\n"),
            "- a\n  b\n  c\n+ a\n  d\n"
        );
        assert_eq!(
            diff("p\nq\nr\ns\n", "q\np\ns\nr\n"),
            "- p\n  q\n- r\n+ p\n  s\n+ r\n"
        );
    }
}
