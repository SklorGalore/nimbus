// A fixed sample for the screenshot test: change it, and the reference
// image must be made again (NIMBUS_UPDATE_SCREENSHOT=1).
use std::collections::HashMap;

/// Counts words, ignoring case.
fn count(text: &str) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for word in text.split_whitespace() {
        *counts.entry(word.to_lowercase()).or_insert(0) += 1;
    }
    counts
}

fn main() {
    let n = count("the quick brown fox — über naïve café");
    println!("{} words, {:?}", n.len(), [1, 2, 3]);
}
