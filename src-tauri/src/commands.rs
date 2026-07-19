#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandAction {
    Paste,
    Dispatch,
    Execute,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParsedCommand {
    pub body: String,
    pub action: CommandAction,
}

pub fn parse(transcript: &str, enabled: bool) -> ParsedCommand {
    let trimmed = transcript.trim();
    if !enabled {
        return ParsedCommand {
            body: trimmed.to_string(),
            action: CommandAction::Paste,
        };
    }

    let without_punctuation = trimmed.trim_end_matches(|character: char| {
        character.is_whitespace() || matches!(character, '.' | ',' | '!' | '?' | ':' | ';')
    });
    let word_start = without_punctuation
        .char_indices()
        .rev()
        .find_map(|(index, ch)| ch.is_whitespace().then_some(index + ch.len_utf8()))
        .unwrap_or(0);
    let word = &without_punctuation[word_start..];
    let action = if word.eq_ignore_ascii_case("dispatch") {
        Some(CommandAction::Dispatch)
    } else if word.eq_ignore_ascii_case("execute") {
        Some(CommandAction::Execute)
    } else {
        None
    };

    match action {
        Some(action) => ParsedCommand {
            body: without_punctuation[..word_start].trim_end().to_string(),
            action,
        },
        None => ParsedCommand {
            body: trimmed.to_string(),
            action: CommandAction::Paste,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_terminal_commands_case_insensitively() {
        assert_eq!(
            parse("search for otters DISPATCH!!!  ", true),
            ParsedCommand {
                body: "search for otters".into(),
                action: CommandAction::Dispatch,
            }
        );
        assert_eq!(
            parse("what is ten times 52 Execute ? ", true),
            ParsedCommand {
                body: "what is ten times 52".into(),
                action: CommandAction::Execute,
            }
        );
    }

    #[test]
    fn ignores_embedded_and_non_terminal_words() {
        assert_eq!(
            parse("dispatch this later", true).action,
            CommandAction::Paste
        );
        assert_eq!(
            parse("execute the plan tomorrow", true).action,
            CommandAction::Paste
        );
        assert_eq!(parse("redispatch", true).action, CommandAction::Paste);
    }

    #[test]
    fn disabled_commands_are_ordinary_dictation() {
        assert_eq!(
            parse("hello dispatch", false),
            ParsedCommand {
                body: "hello dispatch".into(),
                action: CommandAction::Paste,
            }
        );
    }

    #[test]
    fn command_only_has_an_empty_body() {
        assert_eq!(parse(" execute... ", true).body, "");
    }

    #[test]
    fn preserves_punctuation_before_the_command() {
        assert_eq!(parse("Hello there. dispatch", true).body, "Hello there.");
    }
}
