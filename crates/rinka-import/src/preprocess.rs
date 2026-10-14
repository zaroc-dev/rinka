//! A small C preprocessor for Wallpaper Engine shaders.
//!
//! It resolves `#include` and conditional compilation (`#if`, `#ifdef`,
//! `#elif`, …) so declarations can be rewritten on the text that will
//! actually compile. `#define`/`#undef` lines in active regions are kept for
//! naga, which performs macro expansion in the code itself.

use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum PreprocessError {
    #[error("include {0:?} not found")]
    Include(String),
    #[error("include nesting too deep at {0:?}")]
    Depth(String),
    #[error("malformed #if expression {0:?}")]
    Expression(String),
    #[error("unbalanced conditional directives")]
    Unbalanced,
}

#[derive(Clone, Debug)]
struct Macro {
    params: Option<Vec<String>>,
    body: String,
}

pub struct Preprocessor<'a> {
    macros: HashMap<String, Macro>,
    include: &'a dyn Fn(&str) -> Option<String>,
    /// Headers are included once (they have no include guards).
    included: std::collections::HashSet<String>,
}

impl<'a> Preprocessor<'a> {
    pub fn new(include: &'a dyn Fn(&str) -> Option<String>) -> Self {
        Self {
            macros: HashMap::new(),
            include,
            included: Default::default(),
        }
    }

    pub fn define(&mut self, name: &str, value: &str) {
        self.macros.insert(
            name.to_owned(),
            Macro {
                params: None,
                body: value.to_owned(),
            },
        );
    }

    /// Resolves includes and conditionals; returns the active text.
    pub fn run(&mut self, source: &str) -> Result<String, PreprocessError> {
        let mut out = String::new();
        self.process(source, &mut out, 0)?;
        Ok(out)
    }

    fn process(
        &mut self,
        source: &str,
        out: &mut String,
        depth: usize,
    ) -> Result<(), PreprocessError> {
        // Each frame: (this branch active, some branch already taken, parent active).
        let mut stack: Vec<(bool, bool, bool)> = Vec::new();
        let active = |stack: &[(bool, bool, bool)]| stack.last().is_none_or(|f| f.0 && f.2);
        let joined = join_continuations(&strip_block_comments(source));
        for line in joined.lines() {
            let trimmed = line.trim_start();
            let Some(directive) = trimmed.strip_prefix('#') else {
                if active(&stack) {
                    out.push_str(line);
                    out.push('\n');
                }
                continue;
            };
            let directive = strip_line_comment(directive).trim();
            let (name, rest) = directive
                .split_once(|c: char| c.is_whitespace())
                .map_or((directive, ""), |(a, b)| (a, b.trim()));
            let parent = active(&stack);
            match name {
                "if" => {
                    let v = parent && self.eval(rest)?;
                    stack.push((v, v, parent));
                }
                "ifdef" | "ifndef" => {
                    let defined = self
                        .macros
                        .contains_key(rest.split_whitespace().next().unwrap_or(""));
                    let v = parent && (defined == (name == "ifdef"));
                    stack.push((v, v, parent));
                }
                "elif" => {
                    let frame = stack.last_mut().ok_or(PreprocessError::Unbalanced)?;
                    let taken = frame.1;
                    let v = !taken && frame.2 && self.eval(rest)?;
                    let frame = stack.last_mut().expect("frame");
                    frame.0 = v;
                    frame.1 |= v;
                }
                "else" => {
                    let frame = stack.last_mut().ok_or(PreprocessError::Unbalanced)?;
                    frame.0 = !frame.1;
                    frame.1 = true;
                }
                "endif" => {
                    stack.pop().ok_or(PreprocessError::Unbalanced)?;
                }
                _ if !parent => {}
                "include" => {
                    let path = rest.trim_matches(|c| c == '"' || c == '<' || c == '>');
                    if depth > 16 {
                        return Err(PreprocessError::Depth(path.to_owned()));
                    }
                    if !self.included.insert(path.to_owned()) {
                        continue;
                    }
                    let text = (self.include)(path)
                        .ok_or_else(|| PreprocessError::Include(path.to_owned()))?;
                    self.process(&text, out, depth + 1)?;
                }
                "define" => {
                    self.record_define(rest);
                    out.push_str(&format!("#define {rest}\n"));
                }
                "undef" => {
                    self.macros.remove(rest.trim());
                    out.push_str(&format!("#undef {rest}\n"));
                }
                // #version, #extension, #pragma, #line: the translator emits
                // its own header.
                _ => {}
            }
        }
        if stack.is_empty() {
            Ok(())
        } else {
            Err(PreprocessError::Unbalanced)
        }
    }

    fn record_define(&mut self, rest: &str) {
        let name_end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        let after = &rest[name_end..];
        let (params, body) = if let Some(args) = after.strip_prefix('(') {
            let close = args.find(')').unwrap_or(args.len());
            let params = args[..close]
                .split(',')
                .map(|p| p.trim().to_owned())
                .filter(|p| !p.is_empty())
                .collect();
            (
                Some(params),
                args.get(close + 1..).unwrap_or("").trim().to_owned(),
            )
        } else {
            (None, after.trim().to_owned())
        };
        self.macros.insert(name.to_owned(), Macro { params, body });
    }

    fn eval(&self, expr: &str) -> Result<bool, PreprocessError> {
        let tokens = self.expand(&tokenize(expr), 0);
        let mut p = ExprParser {
            tokens: &tokens,
            pos: 0,
        };
        let v = p
            .ternary()
            .ok_or_else(|| PreprocessError::Expression(expr.to_owned()))?;
        if p.pos != tokens.len() {
            return Err(PreprocessError::Expression(expr.to_owned()));
        }
        Ok(v != 0)
    }

    /// Expands `defined` and object-like macros; unknown identifiers are 0.
    fn expand(&self, tokens: &[String], depth: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            let t = &tokens[i];
            if t == "defined" {
                let (name, skip) = if tokens.get(i + 1).map(String::as_str) == Some("(") {
                    (tokens.get(i + 2).cloned().unwrap_or_default(), 4)
                } else {
                    (tokens.get(i + 1).cloned().unwrap_or_default(), 2)
                };
                out.push(
                    if self.macros.contains_key(&name) {
                        "1"
                    } else {
                        "0"
                    }
                    .to_owned(),
                );
                i += skip;
                continue;
            }
            if is_ident(t) {
                match self.macros.get(t) {
                    Some(m) if m.params.is_none() && depth < 32 => {
                        out.extend(self.expand(&tokenize(&m.body), depth + 1));
                    }
                    _ => out.push("0".to_owned()),
                }
            } else {
                out.push(t.clone());
            }
            i += 1;
        }
        out
    }
}

fn is_ident(t: &str) -> bool {
    t.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        } else {
            let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
            if ["&&", "||", "==", "!=", "<=", ">=", "<<", ">>"].contains(&two.as_str()) {
                out.push(two);
                i += 2;
            } else {
                out.push(c.to_string());
                i += 1;
            }
        }
    }
    out
}

struct ExprParser<'a> {
    tokens: &'a [String],
    pos: usize,
}

impl ExprParser<'_> {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(String::as_str)
    }

    fn ternary(&mut self) -> Option<i64> {
        let c = self.binary(0)?;
        if self.peek() == Some("?") {
            self.pos += 1;
            let a = self.ternary()?;
            (self.peek() == Some(":")).then_some(())?;
            self.pos += 1;
            let b = self.ternary()?;
            return Some(if c != 0 { a } else { b });
        }
        Some(c)
    }

    fn binary(&mut self, min_prec: u8) -> Option<i64> {
        let mut lhs = self.unary()?;
        while let Some(op) = self.peek() {
            let prec = match op {
                "||" => 1,
                "&&" => 2,
                "|" => 3,
                "^" => 4,
                "&" => 5,
                "==" | "!=" => 6,
                "<" | ">" | "<=" | ">=" => 7,
                "<<" | ">>" => 8,
                "+" | "-" => 9,
                "*" | "/" | "%" => 10,
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            let op = op.to_owned();
            self.pos += 1;
            let rhs = self.binary(prec + 1)?;
            lhs = match op.as_str() {
                "||" => i64::from(lhs != 0 || rhs != 0),
                "&&" => i64::from(lhs != 0 && rhs != 0),
                "|" => lhs | rhs,
                "^" => lhs ^ rhs,
                "&" => lhs & rhs,
                "==" => i64::from(lhs == rhs),
                "!=" => i64::from(lhs != rhs),
                "<" => i64::from(lhs < rhs),
                ">" => i64::from(lhs > rhs),
                "<=" => i64::from(lhs <= rhs),
                ">=" => i64::from(lhs >= rhs),
                "<<" => lhs << rhs,
                ">>" => lhs >> rhs,
                "+" => lhs + rhs,
                "-" => lhs - rhs,
                "*" => lhs * rhs,
                "/" => lhs.checked_div(rhs)?,
                "%" => lhs.checked_rem(rhs)?,
                _ => unreachable!(),
            };
        }
        Some(lhs)
    }

    fn unary(&mut self) -> Option<i64> {
        match self.peek()? {
            "!" => {
                self.pos += 1;
                Some(i64::from(self.unary()? == 0))
            }
            "-" => {
                self.pos += 1;
                Some(-self.unary()?)
            }
            "+" => {
                self.pos += 1;
                self.unary()
            }
            "(" => {
                self.pos += 1;
                let v = self.ternary()?;
                (self.peek() == Some(")")).then_some(())?;
                self.pos += 1;
                Some(v)
            }
            t => {
                let t = t.trim_end_matches(['u', 'U', 'l', 'L']);
                let v = t
                    .parse::<i64>()
                    .ok()
                    .or_else(|| t.parse::<f64>().ok().map(|f| f as i64))?;
                self.pos += 1;
                Some(v)
            }
        }
    }
}

fn strip_block_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("*/") {
            Some(end) => {
                // Keep line structure for directives.
                out.extend(after[..end].chars().filter(|&c| c == '\n'));
                rest = &after[end + 2..];
            }
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn join_continuations(s: &str) -> String {
    s.replace("\\\r\n", "").replace("\\\n", "")
}

fn strip_line_comment(s: &str) -> &str {
    s.split_once("//").map_or(s, |(a, _)| a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str, defines: &[(&str, &str)]) -> String {
        let include = |name: &str| (name == "common.h").then(|| "float common;\n".to_owned());
        let mut p = Preprocessor::new(&include);
        for (k, v) in defines {
            p.define(k, v);
        }
        p.run(src).unwrap()
    }

    #[test]
    fn conditionals_and_includes() {
        let src = "#include \"common.h\"\n#if MASK == 1\nA\n#elif defined(OTHER) || X > 2\nB\n#else\nC\n#endif\n#ifdef MASK\nD\n#endif\n";
        assert_eq!(run(src, &[("MASK", "1")]), "float common;\nA\nD\n");
        assert_eq!(run(src, &[("X", "3")]), "float common;\nB\n");
        assert_eq!(run(src, &[]), "float common;\nC\n");
    }

    #[test]
    fn nested_inactive_regions_stay_inactive() {
        let src = "#if 0\n#if 1\nA\n#else\nB\n#endif\n#else\nC\n#endif\n";
        assert_eq!(run(src, &[]), "C\n");
    }

    #[test]
    fn defines_feed_later_conditionals() {
        let src = "#define LEVEL (1 + 1)\n#if LEVEL == 2 && !defined(NOPE)\nyes\n#endif\n";
        assert_eq!(run(src, &[]), "#define LEVEL (1 + 1)\nyes\n");
    }
}
