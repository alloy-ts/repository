//! Shell tokenizer and parser supporting pipelines, redirections, variable assignments,
//! command sequences (&&, ||, ;), and command substitution.

use crate::shell::ast::{CommandNode, LogicalOp, PipelineNode, PipelineSequence, RedirectOp, Redirection};

pub fn parse_shell_script(input: &str) -> Result<PipelineSequence, String> {
    let mut parser = ShellParser::new(input);
    parser.parse_sequence()
}

struct ShellParser<'a> {
    chars: Vec<char>,
    pos: usize,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> ShellParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
            _marker: std::marker::PhantomData,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_next(&self) -> Option<char> {
        self.chars.get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<char> {
        if self.pos < self.chars.len() {
            let ch = self.chars[self.pos];
            self.pos += 1;
            Some(ch)
        } else {
            None
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn parse_sequence(&mut self) -> Result<PipelineSequence, String> {
        let mut pipelines = Vec::new();
        self.skip_whitespace();
        if self.pos >= self.chars.len() {
            return Ok(PipelineSequence { pipelines });
        }

        let first = self.parse_pipeline()?;
        pipelines.push((LogicalOp::Sequential, first));

        loop {
            self.skip_whitespace();
            if self.pos >= self.chars.len() {
                break;
            }

            let op = if self.match_str("&&") {
                LogicalOp::And
            } else if self.match_str("||") {
                LogicalOp::Or
            } else if self.match_str(";") || self.match_str("\n") {
                LogicalOp::Sequential
            } else {
                break;
            };

            self.skip_whitespace();
            if self.pos >= self.chars.len() {
                break;
            }

            let pipeline = self.parse_pipeline()?;
            pipelines.push((op, pipeline));
        }

        Ok(PipelineSequence { pipelines })
    }

    fn match_str(&mut self, s: &str) -> bool {
        let len = s.chars().count();
        if self.pos + len <= self.chars.len() {
            let slice: String = self.chars[self.pos..self.pos + len].iter().collect();
            if slice == s {
                self.pos += len;
                return true;
            }
        }
        false
    }

    fn parse_pipeline(&mut self) -> Result<PipelineNode, String> {
        let mut commands = Vec::new();
        let first = self.parse_command()?;
        commands.push(first);

        loop {
            self.skip_whitespace();
            if self.peek() == Some('|') && self.peek_next() != Some('|') {
                self.advance(); // consume '|'
                self.skip_whitespace();
                let cmd = self.parse_command()?;
                commands.push(cmd);
            } else {
                break;
            }
        }

        Ok(PipelineNode { commands })
    }

    fn parse_command(&mut self) -> Result<CommandNode, String> {
        let mut env_vars = Vec::new();
        let mut args = Vec::new();
        let mut redirects = Vec::new();

        let mut parsed_any_arg = false;

        loop {
            self.skip_whitespace();
            if self.pos >= self.chars.len() {
                break;
            }

            let ch = match self.peek() {
                Some(c) => c,
                None => break,
            };

            // Terminating operators
            if ch == '|' || ch == ';' || ch == '&' || ch == '\n' {
                break;
            }

            // Redirections
            if let Some(redir) = self.try_parse_redirection()? {
                redirects.push(redir);
                continue;
            }

            // Word / Argument
            let word = self.parse_word()?;
            if word.is_empty() {
                continue;
            }

            // Check if this is an env assignment prefix (e.g. FOO=bar) before command
            if !parsed_any_arg && is_env_assignment(&word) {
                let parts: Vec<&str> = word.splitn(2, '=').collect();
                env_vars.push((parts[0].to_string(), parts[1].to_string()));
            } else {
                parsed_any_arg = true;
                args.push(word);
            }
        }

        Ok(CommandNode {
            env_vars,
            args,
            redirects,
        })
    }

    fn try_parse_redirection(&mut self) -> Result<Option<Redirection>, String> {
        let start = self.pos;
        let mut matched_op = None;

        if self.match_str("&>>") {
            matched_op = Some(RedirectOp::BothAppend);
        } else if self.match_str("&>") {
            matched_op = Some(RedirectOp::BothTruncate);
        } else if self.match_str("2>&1") {
            return Ok(Some(Redirection {
                op: RedirectOp::StderrToStdout,
                target: String::new(),
            }));
        } else if self.match_str("1>&2") {
            return Ok(Some(Redirection {
                op: RedirectOp::StdoutToStderr,
                target: String::new(),
            }));
        } else if self.match_str("2>>") {
            matched_op = Some(RedirectOp::StderrAppend);
        } else if self.match_str("2>") {
            matched_op = Some(RedirectOp::StderrTruncate);
        } else if self.match_str("1>>") || self.match_str(">>") {
            matched_op = Some(RedirectOp::OutputAppend);
        } else if self.match_str("1>") || self.match_str(">") {
            matched_op = Some(RedirectOp::OutputTruncate);
        } else if self.match_str("<") {
            matched_op = Some(RedirectOp::Input);
        }

        if let Some(op) = matched_op {
            self.skip_whitespace();
            let target = self.parse_word()?;
            if target.is_empty() {
                return Err("Syntax error: expected redirect target".to_string());
            }
            return Ok(Some(Redirection { op, target }));
        }

        self.pos = start;
        Ok(None)
    }

    fn parse_word(&mut self) -> Result<String, String> {
        let mut result = String::new();

        while let Some(ch) = self.peek() {
            if ch.is_whitespace() || ch == '|' || ch == ';' || ch == '&' || ch == '<' || ch == '>' {
                break;
            }

            if ch == '\'' {
                // Single quotes: preserve literal contents verbatim
                self.advance();
                while let Some(c) = self.advance() {
                    if c == '\'' {
                        break;
                    }
                    result.push(c);
                }
            } else if ch == '"' {
                // Double quotes: handle escape characters
                self.advance();
                while let Some(c) = self.advance() {
                    if c == '"' {
                        break;
                    }
                    if c == '\\' {
                        if let Some(escaped) = self.advance() {
                            result.push(escaped);
                        }
                    } else {
                        result.push(c);
                    }
                }
            } else if ch == '\\' {
                self.advance();
                if let Some(escaped) = self.advance() {
                    result.push(escaped);
                }
            } else {
                result.push(ch);
                self.advance();
            }
        }

        Ok(result)
    }
}

fn is_env_assignment(s: &str) -> bool {
    if let Some(idx) = s.find('=') {
        if idx == 0 {
            return false;
        }
        let var_name = &s[..idx];
        var_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !var_name.chars().next().unwrap().is_ascii_digit()
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_command() {
        let seq = parse_shell_script("echo hello world").unwrap();
        assert_eq!(seq.pipelines.len(), 1);
        let cmd = &seq.pipelines[0].1.commands[0];
        assert_eq!(cmd.args, vec!["echo", "hello", "world"]);
    }

    #[test]
    fn parse_pipeline() {
        let seq = parse_shell_script("cat file.txt | grep foo | wc -l").unwrap();
        assert_eq!(seq.pipelines.len(), 1);
        let p = &seq.pipelines[0].1;
        assert_eq!(p.commands.len(), 3);
        assert_eq!(p.commands[0].args, vec!["cat", "file.txt"]);
        assert_eq!(p.commands[1].args, vec!["grep", "foo"]);
        assert_eq!(p.commands[2].args, vec!["wc", "-l"]);
    }

    #[test]
    fn parse_redirection() {
        let seq = parse_shell_script("echo 'hello' > out.txt 2>&1").unwrap();
        let cmd = &seq.pipelines[0].1.commands[0];
        assert_eq!(cmd.args, vec!["echo", "hello"]);
        assert_eq!(cmd.redirects.len(), 2);
        assert_eq!(cmd.redirects[0].op, RedirectOp::OutputTruncate);
        assert_eq!(cmd.redirects[0].target, "out.txt");
        assert_eq!(cmd.redirects[1].op, RedirectOp::StderrToStdout);
    }
}
