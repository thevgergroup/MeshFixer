//! [`parse_length`]: the one length parser every field, CLI flag and spec
//! value goes through (ADR-0006 rule 1b). A recursive-descent expression
//! grammar over `+ - * /`, parentheses, fractions (`1/8`), and unit
//! suffixes (`mm`, `cm`, `m`, `in`, `ft`, `"`, `'`); a bare number means
//! the caller's default unit. Every term is converted to millimetres as
//! it's parsed, so `5 cm + 2 mm` adds correctly even though the two
//! terms have different units.
//!
//! Errors are typed ([`ParseError`]); there is no code path that returns
//! 0 for invalid input — an error propagates instead, so a caller must
//! decide what to show, never silently zero a person's typed value.

use crate::length::Length;
use crate::unit::Unit;

/// Why [`parse_length`] rejected the input. Every variant names where the
/// trouble was, so an error message can point at it; none of them are
/// ever converted into a default length.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseError {
    #[error("empty input")]
    Empty,
    #[error("'{0}' is not a number")]
    BadNumber(String),
    #[error("'{0}' is not a unit (expected mm, cm, m, in, ft, \" or ')")]
    BadUnit(String),
    #[error("division by zero")]
    DivideByZero,
    #[error("expected ')'")]
    ExpectedCloseParen,
    #[error("unexpected '{0}'")]
    Unexpected(char),
    #[error("unexpected end of input")]
    UnexpectedEnd,
    #[error("a unit on both sides of '*' or '/' (e.g. 'in * in') is not a length")]
    UnitTimesUnit,
}

/// Parses a length expression, such as `5 cm + 2 mm`, `1/8 in`, `(3 + 2)
/// * 2 in`, or a bare `10` (meaning `default_unit`). Returns the result
/// as a [`Length`] in `default_unit`. Never returns 0 for input that
/// isn't literally zero: invalid input is always `Err`.
pub fn parse_length(text: &str, default_unit: Unit) -> Result<Length, ParseError> {
    if text.trim().is_empty() {
        return Err(ParseError::Empty);
    }
    let mut p = Parser {
        chars: text.chars().collect(),
        pos: 0,
    };
    let result = p.expr()?;
    p.skip_ws();
    if let Some(c) = p.peek() {
        return Err(ParseError::Unexpected(c));
    }
    // A `Lengthy` result is already in mm; a `Plain` one (no unit
    // appeared anywhere in the expression, e.g. a bare "10") means
    // `default_unit` by definition, so its raw value is already in that
    // unit, not mm.
    let value = match result {
        Val::Lengthy(mm) => mm / default_unit.mm_per(),
        Val::Plain(v) => v,
    };
    Ok(Length::new(value, default_unit))
}

/// A term's value in millimetres, tracked alongside whether it carries a
/// unit (`Lengthy`, e.g. "5mm") or is dimensionless (`Plain`, e.g. the
/// `2` in `2 * 5mm`). Keeping the two apart is what makes `in * in` a
/// rejected expression: a unit times a unit isn't a length, but a unit
/// times a plain scalar is.
#[derive(Clone, Copy)]
enum Val {
    Plain(f64),
    Lengthy(f64),
}

impl Val {
    fn mm(self) -> f64 {
        match self {
            Val::Plain(v) | Val::Lengthy(v) => v,
        }
    }

    fn is_lengthy(self) -> bool {
        matches!(self, Val::Lengthy(_))
    }

    fn map(self, f: impl FnOnce(f64) -> f64) -> Val {
        match self {
            Val::Plain(v) => Val::Plain(f(v)),
            Val::Lengthy(v) => Val::Lengthy(f(v)),
        }
    }
}

/// Combines two values from `+`/`-`, where the result is lengthy if
/// either side is (so `5mm + 2` or `5 + 2mm` both carry the unit
/// forward; a bare number added to a length just extends it in mm,
/// consistent with [`parse_length`] treating a lone bare number as
/// `default_unit`, i.e. already a length by the time arithmetic sees
/// it at the top level).
fn combine_additive(lhs: Val, rhs_mm: f64, rhs_lengthy: bool, op: fn(f64, f64) -> f64) -> Val {
    let lengthy = lhs.is_lengthy() || rhs_lengthy;
    let v = op(lhs.mm(), rhs_mm);
    if lengthy {
        Val::Lengthy(v)
    } else {
        Val::Plain(v)
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
    }

    /// `expr := term (('+' | '-') term)*`, folded left to right.
    fn expr(&mut self) -> Result<Val, ParseError> {
        let mut acc = self.term()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('+') => {
                    self.bump();
                    let rhs = self.term()?;
                    acc = combine_additive(acc, rhs.mm(), rhs.is_lengthy(), |a, b| a + b);
                }
                Some('-') => {
                    self.bump();
                    let rhs = self.term()?;
                    acc = combine_additive(acc, rhs.mm(), rhs.is_lengthy(), |a, b| a - b);
                }
                _ => break,
            }
        }
        Ok(acc)
    }

    /// `term := factor (('*' | '/') factor)*`. A unit times/divided by a
    /// unit is rejected (`in * in` isn't a length); a unit times/divided
    /// by a plain number stays a length, scaled.
    fn term(&mut self) -> Result<Val, ParseError> {
        let mut acc = self.factor()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('*') => {
                    self.bump();
                    let rhs = self.factor()?;
                    if acc.is_lengthy() && rhs.is_lengthy() {
                        return Err(ParseError::UnitTimesUnit);
                    }
                    let lengthy = acc.is_lengthy() || rhs.is_lengthy();
                    let v = acc.mm() * rhs.mm();
                    acc = if lengthy {
                        Val::Lengthy(v)
                    } else {
                        Val::Plain(v)
                    };
                }
                Some('/') => {
                    self.bump();
                    let rhs = self.factor()?;
                    if acc.is_lengthy() && rhs.is_lengthy() {
                        return Err(ParseError::UnitTimesUnit);
                    }
                    if rhs.mm() == 0.0 {
                        return Err(ParseError::DivideByZero);
                    }
                    let lengthy = acc.is_lengthy() || rhs.is_lengthy();
                    let v = acc.mm() / rhs.mm();
                    acc = if lengthy {
                        Val::Lengthy(v)
                    } else {
                        Val::Plain(v)
                    };
                }
                _ => break,
            }
        }
        Ok(acc)
    }

    /// `factor := '(' expr ')' [unit] | ['-'|'+'] number '/' number
    /// [unit] | ['-'|'+'] number [unit]`.
    ///
    /// The fraction form (`1/8 in`) is recognised directly here, ahead
    /// of `term`'s own `/` operator, so a unit suffix with no space
    /// (`1/8in`) attaches to the whole fraction rather than just the
    /// denominator. Plain division written with spaces (`8 / 2 in`)
    /// still produces the same numeric result either way, since both
    /// paths divide two numbers and attach a trailing unit to the
    /// quotient.
    fn factor(&mut self) -> Result<Val, ParseError> {
        self.skip_ws();
        match self.peek() {
            Some('(') => {
                self.bump();
                let inner = self.expr()?;
                self.skip_ws();
                if self.peek() != Some(')') {
                    return Err(ParseError::ExpectedCloseParen);
                }
                self.bump();
                self.skip_ws();
                // A unit suffix right after ')' applies to the group's
                // own numeric result, same as a plain number's own
                // suffix: "(1 + 2) in" is 3 inches (the parenthesised
                // arithmetic was unitless, so its raw value of 3 becomes
                // 3 inches). With no suffix, the group keeps whatever
                // lengthiness its contents already had (e.g. "(5mm +
                // 2mm) * 2" is still a length, unaffected here).
                return match self.try_unit_suffix()? {
                    Some(unit) => Ok(Val::Lengthy(inner.mm() * unit.mm_per())),
                    None => Ok(inner),
                };
            }
            Some('-') => {
                self.bump();
                return Ok(self.factor()?.map(|v| -v));
            }
            Some('+') => {
                self.bump();
                return self.factor();
            }
            _ => {}
        }
        let number = self.number()?;
        self.skip_ws();
        // A fraction: "1/8 in" or "1/8in" — a '/' immediately followed by
        // a digit, with no operator semantics (handled before any unit
        // suffix is read, so the unit applies to the whole fraction).
        if self.peek() == Some('/') {
            let save = self.pos;
            self.bump();
            self.skip_ws();
            if matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.') {
                let denom = self.number()?;
                if denom == 0.0 {
                    return Err(ParseError::DivideByZero);
                }
                let frac = number / denom;
                self.skip_ws();
                return Ok(match self.try_unit_suffix()? {
                    Some(unit) => Val::Lengthy(frac * unit.mm_per()),
                    None => Val::Plain(frac),
                });
            }
            // Not actually a fraction (e.g. "8 / x"): rewind and let
            // `term`'s own '/' operator handle it instead.
            self.pos = save;
        }
        Ok(match self.try_unit_suffix()? {
            Some(unit) => Val::Lengthy(number * unit.mm_per()),
            None => Val::Plain(number),
        })
    }

    /// Reads a number: digits, an optional decimal point. `.5` and `5.`
    /// are both accepted.
    fn number(&mut self) -> Result<f64, ParseError> {
        self.skip_ws();
        let start = self.pos;
        let mut saw_digit = false;
        if matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.') {
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                saw_digit = true;
                self.bump();
            }
            if self.peek() == Some('.') {
                self.bump();
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    saw_digit = true;
                    self.bump();
                }
            }
        }
        if !saw_digit {
            return match self.peek() {
                Some(c) => Err(ParseError::Unexpected(c)),
                None => Err(ParseError::UnexpectedEnd),
            };
        }
        let raw: String = self.chars[start..self.pos].iter().collect();
        raw.parse::<f64>()
            .map_err(|_| ParseError::BadNumber(raw.clone()))
    }

    /// Reads a unit suffix if one follows (`mm`, `cm`, `m`, `in`, `ft`,
    /// `"`, `'`), or `None` if the next token isn't a letter/quote (so a
    /// bare number stays unitless here). A letter sequence that isn't a
    /// recognised unit is a named [`ParseError::BadUnit`], not silently
    /// ignored — "10 xyz" must fail, not quietly parse as "10
    /// default_unit".
    fn try_unit_suffix(&mut self) -> Result<Option<Unit>, ParseError> {
        self.skip_ws();
        match self.peek() {
            Some('"') => {
                self.bump();
                return Ok(Some(Unit::Inch));
            }
            Some('\'') => {
                self.bump();
                return Ok(Some(Unit::Foot));
            }
            Some(c) if c.is_alphabetic() => {}
            _ => return Ok(None),
        }
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_alphabetic()) {
            self.bump();
        }
        let word: String = self.chars[start..self.pos].iter().collect();
        Unit::parse_symbol(&word)
            .map(Some)
            .ok_or(ParseError::BadUnit(word))
    }
}
