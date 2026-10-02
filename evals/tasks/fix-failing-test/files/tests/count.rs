use wordcount::{top_words, word_count};

#[test]
fn counts_words_separated_by_any_whitespace() {
    assert_eq!(word_count("one two  three\nfour\tfive "), 5);
    assert_eq!(word_count("   "), 0);
    assert_eq!(word_count(""), 0);
}

#[test]
fn top_words_are_ordered_by_frequency() {
    let top = top_words("b a b c a b", 2);
    assert_eq!(top, vec![("b".to_string(), 3), ("a".to_string(), 2)]);
}
