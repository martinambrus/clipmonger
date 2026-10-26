//! Case-insensitive substring filters over clips and setting keys.

use clipmonger_core::model::Clip;

/// Filter clip indices by query, preserving input order.
#[must_use]
pub fn filter_clips(clips: &[Clip], query: &str) -> Vec<usize> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return (0..clips.len()).collect();
    }
    clips
        .iter()
        .enumerate()
        .filter(|(_, clip)| {
            let hay = format!("{}\n{}", clip.search_text, clip.preview).to_lowercase();
            hay.contains(&q)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Filter setting-key indices by query with the same match rule.
#[must_use]
pub fn filter_settings(keys: &[&str], query: &str) -> Vec<usize> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return (0..keys.len()).collect();
    }
    keys.iter()
        .enumerate()
        .filter(|(_, key)| key.to_lowercase().contains(&q))
        .map(|(index, _)| index)
        .collect()
}
