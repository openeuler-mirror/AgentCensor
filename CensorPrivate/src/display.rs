//! Stateful restoration of passive display text, shared by JSON and SSE.
use crate::{
    error::{Error, Result},
    references::TOKEN_RE,
};
use regex::Regex;
use std::{collections::VecDeque, sync::LazyLock};

static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:https?|ftp|file|ssh|wss?|mailto|tel|sms|javascript|data|magnet):/{0,2}")
        .expect("constant URL grammar")
});
static URL_END: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:https?|ftp|file|ssh|wss?|mailto|tel|sms|javascript|data|magnet):/{0,2}$")
        .expect("constant URL grammar")
});
static SCRIPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<script(?:\s[^>]*)?>$").expect("constant tag grammar"));
fn terminal_control(ch: char) -> bool {
    (ch < ' ' && !matches!(ch, '\n' | '\r' | '\t')) || ('\u{7f}'..='\u{9f}').contains(&ch)
}

#[derive(Default)]
pub struct DisplayRestorer {
    pending: String,
    tail: VecDeque<char>,
    in_url: bool,
    in_tag: bool,
    in_script: bool,
    in_code: bool,
    link_depth: usize,
    terminal_active: bool,
}
impl DisplayRestorer {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    fn literal(&mut self, text: &str) {
        for ch in text.chars() {
            self.terminal_active |= terminal_control(ch);
            self.tail.push_back(ch);
            if self.tail.len() > 64 {
                self.tail.pop_front();
            }
            if ch.is_whitespace() || matches!(ch, '"' | '\'') {
                self.in_url = false;
            }
            if matches!(ch, ':' | '/') && URL_END.is_match(&self.tail.iter().collect::<String>()) {
                self.in_url = true;
            }
            if ch == '(' && (self.tail.iter().rev().nth(1) == Some(&']') || self.link_depth > 0) {
                self.link_depth += 1;
            } else if self.link_depth > 0 && ch == ')' {
                self.link_depth -= 1;
            }
            if ch == '`' {
                self.in_code = !self.in_code;
            }
            if ch == '<' {
                self.in_tag = true;
            }
            if ch == '>' {
                let tail: String = self.tail.iter().collect();
                if SCRIPT.is_match(&tail) {
                    self.in_script = true;
                }
                if tail.to_ascii_lowercase().ends_with("</script>") {
                    self.in_script = false;
                }
                self.in_tag = false;
            }
        }
    }
    fn token(
        &self,
        token: &str,
        restore: &mut impl FnMut(&str) -> Result<String>,
    ) -> Result<String> {
        if self.terminal_active
            || self.in_url
            || self.in_tag
            || self.in_script
            || self.in_code
            || self.link_depth > 0
        {
            return Ok(token.into());
        }
        let restored = restore(token)?;
        if URL.is_match(&restored)
            || restored
                .chars()
                .any(|ch| matches!(ch, '<' | '>' | '`') || terminal_control(ch))
        {
            return Ok(token.into());
        }
        Ok(restored)
    }
    pub fn feed(
        &mut self,
        fragment: &str,
        mut restore: impl FnMut(&str) -> Result<String>,
    ) -> Result<String> {
        self.pending.push_str(fragment);
        let data = std::mem::take(&mut self.pending);
        let mut output = String::new();
        let mut cursor = 0;
        while cursor < data.len() {
            let remaining = &data[cursor..];
            if remaining.starts_with("<CP_") {
                if let Some(close) = remaining.find('>') {
                    if close + 1 > 512 {
                        return Err(Error::ProtocolBlocked);
                    }
                    let candidate = &remaining[..=close];
                    if TOKEN_RE
                        .find(candidate)
                        .is_some_and(|m| m.as_str() == candidate)
                    {
                        output.push_str(&self.token(candidate, &mut restore)?);
                    } else {
                        self.literal(candidate);
                        output.push_str(candidate);
                    }
                    cursor += close + 1;
                } else {
                    if remaining.len() > 512 {
                        return Err(Error::ProtocolBlocked);
                    }
                    self.pending.push_str(remaining);
                    break;
                }
            } else if remaining.len() < 4 && "<CP_".starts_with(remaining) {
                self.pending.push_str(remaining);
                break;
            } else {
                let width = remaining
                    .chars()
                    .next()
                    .ok_or(Error::ProtocolBlocked)?
                    .len_utf8();
                let end = remaining[width..]
                    .find('<')
                    .map_or(data.len(), |index| cursor + index + width);
                let literal = &data[cursor..end];
                self.literal(literal);
                output.push_str(literal);
                cursor = end;
            }
        }
        Ok(output)
    }
    pub fn finish(&mut self) -> Result<String> {
        if self.pending.starts_with("<CP_") {
            return Err(Error::ProtocolBlocked);
        }
        let tail = std::mem::take(&mut self.pending);
        self.literal(&tail);
        Ok(tail)
    }
}
