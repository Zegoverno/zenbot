//! Counts words in text.

/// Number of words in `text`. Words are separated by any whitespace.
pub fn word_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.split(' ').count()
}

/// The `n` most frequent words, most frequent first; ties keep first-seen order.
pub fn top_words(text: &str, n: usize) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for w in text.split_whitespace() {
        let w = w.to_lowercase();
        match counts.iter_mut().find(|(k, _)| *k == w) {
            Some((_, c)) => *c += 1,
            None => counts.push((w, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    counts.truncate(n);
    counts
}
