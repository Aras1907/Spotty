use crate::search::{Action, ResultKind, SearchResult};
pub fn evaluate(q: &str) -> Option<SearchResult> {
    if !q.chars().any(|c| c.is_ascii_digit())
        || !q
            .chars()
            .any(|c| matches!(c, '+' | '-' | '*' | '/' | '%' | '('))
    {
        return None;
    }
    let v = ev::parse(q).ok()?;
    let f = if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    Some(SearchResult {
        kind: ResultKind::Calculator,
        title: format!("{q} = {f}"),
        subtitle: Some("Enter to copy".into()),
        icon: Some("accessories-calculator-symbolic".into()),
        action: Action::InsertCalculatorResult(f),
        score: i32::MAX - 1,
    })
}
mod ev {
    pub struct E;
    pub fn parse(s: &str) -> Result<f64, E> {
        let mut p = P {
            s: s.as_bytes(),
            i: 0,
        };
        p.w();
        let v = p.expr()?;
        p.w();
        if p.i != p.s.len() || !v.is_finite() {
            Err(E)
        } else {
            Ok(v)
        }
    }
    struct P<'a> {
        s: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn pk(&self) -> Option<u8> {
            self.s.get(self.i).copied()
        }
        fn w(&mut self) {
            while matches!(self.pk(), Some(b' ' | b'\t')) {
                self.i += 1;
            }
        }
        fn eat(&mut self, b: u8) -> bool {
            self.w();
            if self.pk() == Some(b) {
                self.i += 1;
                true
            } else {
                false
            }
        }
        fn expr(&mut self) -> Result<f64, E> {
            let mut a = self.term()?;
            loop {
                self.w();
                if self.eat(b'+') {
                    a += self.term()?
                } else if self.eat(b'-') {
                    a -= self.term()?
                } else {
                    return Ok(a);
                }
            }
        }
        fn term(&mut self) -> Result<f64, E> {
            let mut a = self.fac()?;
            loop {
                self.w();
                if self.eat(b'*') {
                    a *= self.fac()?
                } else if self.eat(b'/') {
                    let d = self.fac()?;
                    if d == 0.0 {
                        return Err(E);
                    }
                    a /= d
                } else if self.eat(b'%') {
                    a %= self.fac()?
                } else {
                    return Ok(a);
                }
            }
        }
        fn fac(&mut self) -> Result<f64, E> {
            self.w();
            if self.eat(b'-') {
                Ok(-self.fac()?)
            } else if self.eat(b'+') {
                self.fac()
            } else if self.eat(b'(') {
                let v = self.expr()?;
                if !self.eat(b')') {
                    Err(E)
                } else {
                    Ok(v)
                }
            } else {
                self.num()
            }
        }
        fn num(&mut self) -> Result<f64, E> {
            self.w();
            let s = self.i;
            while let Some(c) = self.pk() {
                if c.is_ascii_digit() || c == b'.' {
                    self.i += 1
                } else {
                    break;
                }
            }
            if self.i == s {
                Err(E)
            } else {
                std::str::from_utf8(&self.s[s..self.i])
                    .map_err(|_| E)?
                    .parse()
                    .map_err(|_| E)
            }
        }
    }
}
