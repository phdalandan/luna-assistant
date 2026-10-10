//! Spells any wake word in the keyword model's word pieces, as its sentencepiece unigram tokenizer
//! would, using the piece scores in the model's `bpe.model` protobuf. No retraining is needed.
use std::collections::HashMap;

/// The keyword line sherpa-onnx expects, such as `▁ LU N A @WAKE`, or `None` if the name
/// contains anything other than letters, apostrophes, and spaces.
pub fn keyword_line(vocabulary: &[u8], name: &str) -> Option<String> {
    let pieces = encode(&scores(vocabulary)?, name)?;
    Some(format!("{} @WAKE", pieces.join(" ")))
}

/// Letters and apostrophes, at most three words. Checked before a name is saved.
pub fn is_valid_name(name: &str) -> bool {
    let words: Vec<&str> = name.split_whitespace().collect();
    let letters: usize = words.iter().map(|word| word.chars().count()).sum();
    !words.is_empty()
        && words.len() <= 3
        && (2..=24).contains(&letters)
        && words.iter().all(|word| {
            word.chars().any(char::is_alphabetic)
                && word.chars().all(|c| c.is_ascii_alphabetic() || c == '\'')
        })
}

/// The best-scoring segmentation of the name into known pieces.
fn encode(scores: &HashMap<String, f32>, name: &str) -> Option<Vec<String>> {
    if !is_valid_name(name) {
        return None;
    }
    let text: Vec<char> = name
        .split_whitespace()
        .flat_map(|word| {
            let letters: Vec<char> = word.to_uppercase().chars().collect();
            std::iter::once('\u{2581}').chain(letters)
        })
        .collect();
    let longest = scores.keys().map(|piece| piece.chars().count()).max()?;
    // best[end] is the highest total score for text[..end] and where its last piece starts.
    let mut best: Vec<Option<(f32, usize)>> = vec![None; text.len() + 1];
    best[0] = Some((0.0, 0));
    for end in 1..=text.len() {
        for start in end.saturating_sub(longest)..end {
            let Some((before, _)) = best[start] else {
                continue;
            };
            let piece: String = text[start..end].iter().collect();
            if let Some(score) = scores.get(&piece) {
                let total = before + score;
                if best[end].is_none_or(|(current, _)| total > current) {
                    best[end] = Some((total, start));
                }
            }
        }
    }
    let mut pieces = Vec::new();
    let mut end = text.len();
    while end > 0 {
        let (_, start) = best[end]?;
        pieces.push(text[start..end].iter().collect());
        end = start;
    }
    pieces.reverse();
    Some(pieces)
}

/// Normal pieces and their scores from a sentencepiece `ModelProto`.
fn scores(model: &[u8]) -> Option<HashMap<String, f32>> {
    const NORMAL: u64 = 1;
    let mut scores = HashMap::new();
    for (field, value) in fields(model)? {
        let (1, Value::Bytes(piece)) = (field, value) else {
            continue;
        };
        let (mut text, mut score, mut kind) = (None, 0.0, NORMAL);
        for (field, value) in fields(piece)? {
            match (field, value) {
                (1, Value::Bytes(bytes)) => text = Some(std::str::from_utf8(bytes).ok()?),
                (2, Value::Fixed32(bits)) => score = f32::from_bits(bits),
                (3, Value::Varint(value)) => kind = value,
                _ => {}
            }
        }
        if kind == NORMAL {
            scores.insert(text?.to_owned(), score);
        }
    }
    (!scores.is_empty()).then_some(scores)
}

enum Value<'a> {
    Varint(u64),
    Fixed32(u32),
    Bytes(&'a [u8]),
    Other,
}

/// The fields of one protobuf message, or `None` if it is malformed.
fn fields(mut data: &[u8]) -> Option<Vec<(u64, Value<'_>)>> {
    let mut fields = Vec::new();
    while !data.is_empty() {
        let key = varint(&mut data)?;
        let value = match key & 7 {
            0 => Value::Varint(varint(&mut data)?),
            1 => {
                data = data.get(8..)?;
                Value::Other
            }
            2 => {
                let len = usize::try_from(varint(&mut data)?).ok()?;
                let bytes = data.get(..len)?;
                data = &data[len..];
                Value::Bytes(bytes)
            }
            5 => {
                let bytes: [u8; 4] = data.get(..4)?.try_into().ok()?;
                data = &data[4..];
                Value::Fixed32(u32::from_le_bytes(bytes))
            }
            _ => return None,
        };
        fields.push((key >> 3, value));
    }
    Some(fields)
}

fn varint(data: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = data.split_first()?;
        *data = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(fields: &[(u64, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (key, payload) in fields {
            out.push(*key as u8);
            out.extend(payload);
        }
        out
    }

    fn piece(text: &str, score: f32, kind: u8) -> Vec<u8> {
        let mut text_field = vec![text.len() as u8];
        text_field.extend(text.as_bytes());
        let body = message(&[
            ((1 << 3) | 2, text_field),
            ((2 << 3) | 5, score.to_le_bytes().to_vec()),
            (3 << 3, vec![kind]),
        ]);
        let mut field = vec![(1 << 3) | 2, body.len() as u8];
        field.extend(body);
        field
    }

    fn vocabulary() -> Vec<u8> {
        let mut model = Vec::new();
        model.extend(piece("<unk>", 0.0, 2));
        for (text, score) in [
            ("\u{2581}", -3.0),
            ("\u{2581}LU", -9.0),
            ("LU", -5.0),
            ("N", -4.0),
            ("A", -4.0),
            ("NA", -9.5),
            ("L", -6.0),
            ("U", -6.0),
        ] {
            model.extend(piece(text, score, 1));
        }
        // An unrelated top-level field, as the real model has trainer settings.
        model.extend([(2 << 3) | 2, 2, 8, 1]);
        model
    }

    #[test]
    fn picks_the_highest_scoring_pieces() {
        assert_eq!(
            keyword_line(&vocabulary(), "Luna").as_deref(),
            Some("\u{2581} LU N A @WAKE")
        );
    }

    #[test]
    fn names_the_vocabulary_cannot_spell_are_rejected() {
        assert_eq!(keyword_line(&vocabulary(), "Lux"), None);
        assert_eq!(keyword_line(b"not a model", "Luna"), None);
    }

    #[test]
    fn only_short_names_of_letters_are_valid() {
        for name in ["Luna", "Jarvis", "Hey Jarvis", "O'Brien", "Mary Jane"] {
            assert!(is_valid_name(name), "{name}");
        }
        for name in ["", "L", "R2D2", "Luna!", "one two three four", "Lúna"] {
            assert!(!is_valid_name(name), "{name}");
        }
    }
}
