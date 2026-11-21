use std::collections::HashMap;

use notepust_contracts::{ReplacementPiece, ReplacementProgram};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplacementError {
    #[error("unknown replacement escape \\{character}")]
    UnknownEscape { character: char },

    #[error("replacement ends with a backslash")]
    TrailingBackslash,

    #[error("unterminated ${{...}} group reference")]
    UnterminatedGroup,

    #[error("group reference ${reference} is out of range; the pattern has {available} groups")]
    UnknownGroup { reference: String, available: usize },

    #[error("invalid group name {name:?}")]
    InvalidName { name: String },

    #[error("group index ${index} is out of range; valid indexes are 0..={max}")]
    GroupOutOfRange { index: u32, max: u32 },
}

fn push_literal(pieces: &mut Vec<ReplacementPiece>, text: &str) {
    if text.is_empty() {
        return;
    }
    match pieces.last_mut() {
        Some(ReplacementPiece::Literal { text: existing }) => existing.push_str(text),
        _ => pieces.push(ReplacementPiece::Literal {
            text: text.to_string(),
        }),
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|character| character.is_alphanumeric() || character == '_')
}

/// Parses the application replacement language.
///
/// `$0`–`$99`, `${name}`, `$$` and (when enabled) `\n`, `\r`, `\t`, `\\` are
/// recognized. Literal searches do not call this parser; their replacement is
/// copied verbatim.
pub fn parse_replacement(
    input: &str,
    interpret_escapes: bool,
) -> Result<ReplacementProgram, ReplacementError> {
    let mut pieces = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(character) = chars.next() {
        match character {
            '\\' if interpret_escapes => match chars.next() {
                Some('n') => push_literal(&mut pieces, "\n"),
                Some('r') => push_literal(&mut pieces, "\r"),
                Some('t') => push_literal(&mut pieces, "\t"),
                Some('\\') => push_literal(&mut pieces, "\\"),
                Some(other) => return Err(ReplacementError::UnknownEscape { character: other }),
                None => return Err(ReplacementError::TrailingBackslash),
            },
            '$' => match chars.peek().copied() {
                Some('$') => {
                    chars.next();
                    pieces.push(ReplacementPiece::Dollar);
                }
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    let mut closed = false;
                    for next in chars.by_ref() {
                        if next == '}' {
                            closed = true;
                            break;
                        }
                        name.push(next);
                    }
                    if !closed {
                        return Err(ReplacementError::UnterminatedGroup);
                    }
                    if name.is_empty() {
                        return Err(ReplacementError::InvalidName { name });
                    }
                    if name.chars().all(|character| character.is_ascii_digit()) {
                        let index: u32 = name
                            .parse()
                            .map_err(|_| ReplacementError::InvalidName { name: name.clone() })?;
                        if index == 0 {
                            pieces.push(ReplacementPiece::Match);
                        } else {
                            pieces.push(ReplacementPiece::Group { index });
                        }
                    } else if valid_name(&name) {
                        pieces.push(ReplacementPiece::NamedGroup { name });
                    } else {
                        return Err(ReplacementError::InvalidName { name });
                    }
                }
                Some(first) if first.is_ascii_digit() => {
                    let mut digits = String::new();
                    while let Some(next) = chars.peek().copied() {
                        if next.is_ascii_digit() && digits.len() < 2 {
                            digits.push(next);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if let Some(next) = chars.peek().filter(|next| next.is_ascii_digit()) {
                        return Err(ReplacementError::InvalidName {
                            name: format!("${digits}{next}..."),
                        });
                    }
                    let index: u32 = digits.parse().map_err(|_| ReplacementError::InvalidName {
                        name: digits.clone(),
                    })?;
                    if index == 0 {
                        pieces.push(ReplacementPiece::Match);
                    } else {
                        pieces.push(ReplacementPiece::Group { index });
                    }
                }
                _ => push_literal(&mut pieces, "$"),
            },
            other => {
                let mut buffer = String::new();
                buffer.push(other);
                push_literal(&mut pieces, &buffer);
            }
        }
    }

    Ok(ReplacementProgram { pieces })
}

/// Validates group references against the compiled pattern.
pub fn validate_program(
    program: &ReplacementProgram,
    group_count: usize,
    named_groups: &[String],
) -> Result<(), ReplacementError> {
    let mut named: HashMap<&str, ()> = HashMap::new();
    for name in named_groups {
        named.insert(name.as_str(), ());
    }
    let max = group_count.saturating_sub(1) as u32;
    for piece in &program.pieces {
        match piece {
            ReplacementPiece::Group { index } => {
                if *index > max {
                    return Err(ReplacementError::GroupOutOfRange { index: *index, max });
                }
            }
            ReplacementPiece::NamedGroup { name } if !named.contains_key(name.as_str()) => {
                return Err(ReplacementError::UnknownGroup {
                    reference: name.clone(),
                    available: group_count,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Expands a program with explicit group text. Used by worker/PCRE2 paths.
pub fn expand_parts(
    program: &ReplacementProgram,
    whole: &str,
    groups: &[Option<&str>],
    named: &HashMap<String, String>,
) -> String {
    let mut output = String::new();
    for piece in &program.pieces {
        match piece {
            ReplacementPiece::Literal { text } => output.push_str(text),
            ReplacementPiece::Match => output.push_str(whole),
            ReplacementPiece::Group { index } => {
                if let Some(Some(text)) = groups.get(*index as usize) {
                    output.push_str(text);
                }
            }
            ReplacementPiece::NamedGroup { name } => {
                if let Some(text) = named.get(name) {
                    output.push_str(text);
                }
            }
            ReplacementPiece::Dollar => output.push('$'),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_groups_and_named_groups() {
        let program = parse_replacement("$1-${name}-$0", false).unwrap();
        assert_eq!(
            program.pieces,
            vec![
                ReplacementPiece::Group { index: 1 },
                ReplacementPiece::Literal {
                    text: "-".to_string()
                },
                ReplacementPiece::NamedGroup {
                    name: "name".to_string()
                },
                ReplacementPiece::Literal {
                    text: "-".to_string()
                },
                ReplacementPiece::Match,
            ]
        );
    }

    #[test]
    fn escapes_only_apply_when_enabled() {
        let program = parse_replacement(r"a\nb", true).unwrap();
        assert_eq!(
            program.pieces,
            vec![ReplacementPiece::Literal {
                text: "a\nb".to_string()
            }]
        );
        let literal = parse_replacement(r"a\nb", false).unwrap();
        assert_eq!(
            literal.pieces,
            vec![ReplacementPiece::Literal {
                text: r"a\nb".to_string()
            }]
        );
    }

    #[test]
    fn unknown_escape_is_an_error_only_when_interpreting() {
        assert!(matches!(
            parse_replacement(r"\q", true),
            Err(ReplacementError::UnknownEscape { character: 'q' })
        ));
        assert!(parse_replacement(r"\q", false).is_ok());
    }

    #[test]
    fn dollar_handling() {
        let program = parse_replacement("$$ $x $", false).unwrap();
        let expanded = expand_parts(&program, "M", &[Some("M")], &HashMap::new());
        assert_eq!(expanded, "$ $x $");
    }

    #[test]
    fn validates_group_bounds_and_names() {
        let program = parse_replacement("$9", false).unwrap();
        let error = validate_program(&program, 3, &[]).unwrap_err();
        assert!(matches!(error, ReplacementError::GroupOutOfRange { .. }));

        let program = parse_replacement("${missing}", false).unwrap();
        let error = validate_program(&program, 1, &[]).unwrap_err();
        assert!(matches!(error, ReplacementError::UnknownGroup { .. }));

        let program = parse_replacement("$1 ${present}", false).unwrap();
        assert!(validate_program(&program, 3, &["present".to_string()]).is_ok());
    }

    #[test]
    fn unmatched_braces_are_rejected() {
        assert!(matches!(
            parse_replacement("${name", false),
            Err(ReplacementError::UnterminatedGroup)
        ));
    }

    #[test]
    fn no_match_replacement_can_be_empty() {
        let program = parse_replacement("", false).unwrap();
        assert!(program.pieces.is_empty());
    }
}
