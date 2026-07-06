pub fn normalize_phrase(input: &str) -> String {
    let lower = input.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut previous_space = true;

    for ch in lower.chars().flat_map(char_variants) {
        if ch.is_alphanumeric() || is_cyrillic(ch) {
            out.push(ch);
            previous_space = false;
        } else if !previous_space {
            out.push(' ');
            previous_space = true;
        }
    }

    out.trim().to_string()
}

pub fn matches_wake_phrase(text: &str, grammar: &[String]) -> bool {
    let normalized = normalize_phrase(text);
    grammar
        .iter()
        .map(|phrase| normalize_phrase(phrase))
        .any(|phrase| normalized == phrase || normalized.contains(&phrase))
}

pub fn fuzzy_phrase_score(text: &str, phrase: &str) -> Option<f32> {
    let text = normalize_phrase(text);
    let phrase = normalize_phrase(phrase);
    if text.is_empty() || phrase.len() < 5 {
        return None;
    }
    if text == phrase {
        return Some(1.0);
    }
    if text.contains(&phrase) {
        return Some(0.86);
    }

    let phrase_tokens: Vec<&str> = phrase.split_whitespace().collect();
    let text_tokens: Vec<&str> = text.split_whitespace().collect();
    if phrase_tokens.is_empty() || text_tokens.len() < phrase_tokens.len() {
        return None;
    }

    let mut best = 0.0_f32;
    for window in text_tokens.windows(phrase_tokens.len()) {
        let candidate = window.join(" ");
        best = best.max(levenshtein_similarity(&candidate, &phrase));
    }

    (best >= 0.78).then_some(best)
}

pub fn levenshtein_similarity(left: &str, right: &str) -> f32 {
    let left_chars: Vec<char> = left.chars().collect();
    let right_chars: Vec<char> = right.chars().collect();
    let max_len = left_chars.len().max(right_chars.len());
    if max_len == 0 {
        return 1.0;
    }
    let distance = levenshtein_distance(&left_chars, &right_chars);
    1.0 - (distance as f32 / max_len as f32)
}

fn levenshtein_distance(left: &[char], right: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];

    for (i, left_ch) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, right_ch) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_ch != right_ch);
            let insertion = current[j] + 1;
            let deletion = previous[j + 1] + 1;
            current[j + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[right.len()]
}

fn is_cyrillic(ch: char) -> bool {
    ('а'..='я').contains(&ch) || ch == 'ё'
}

fn char_variants(ch: char) -> Vec<char> {
    match ch {
        'ё' => vec!['е'],
        _ => vec![ch],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_cyrillic_case_punctuation_and_yo() {
        assert_eq!(normalize_phrase("  ЭРЁЗ!!! "), "эрез");
    }

    #[test]
    fn accepts_wake_variants_inside_phrase() {
        let grammar = vec!["эрез".into(), "ерез".into(), "эй рез".into()];
        assert!(matches_wake_phrase("Эрез, открой браузер", &grammar));
        assert!(matches_wake_phrase("эй рез открой браузер", &grammar));
        assert!(!matches_wake_phrase("открой браузер", &grammar));
    }

    #[test]
    fn fuzzy_phrase_accepts_small_recognition_errors() {
        assert!(fuzzy_phrase_score("аткрой браузер", "открой браузер").unwrap() > 0.78);
        assert!(
            fuzzy_phrase_score(
                "включи браусер громкость ниже",
                "включи браузер громкость ниже"
            )
            .unwrap()
                > 0.78
        );
        assert!(fuzzy_phrase_score("выключи свет", "открой браузер").is_none());
    }
}
