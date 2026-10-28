//! Decode quoted atoms with KiCad lexer rules while retaining exact source spans.
use kiutils_sexpr::{Atom, CstDocument, Node, ParseError};

pub(crate) fn parse_one(text: &str) -> Result<CstDocument, ParseError> {
    let mut document = kiutils_sexpr::parse_one(text)?;
    let mut pending = document.nodes.iter_mut().collect::<Vec<_>>();
    while let Some(node) = pending.pop() {
        match node {
            Node::List { items, .. } => pending.extend(items),
            Node::Atom {
                atom: Atom::Quoted(value),
                span,
            } => {
                let bytes = text
                    .as_bytes()
                    .get(span.start + 1..span.end - 1)
                    .ok_or(ParseError::UnexpectedToken(span.start))?;
                let mut out = Vec::with_capacity(bytes.len());
                let mut i = 0;
                while i < bytes.len() {
                    let c = bytes[i];
                    i += 1;
                    if matches!(c, b'\n' | b'\r') {
                        return Err(ParseError::UnexpectedToken(span.start));
                    }
                    if c != b'\\' {
                        out.push(c);
                        continue;
                    }
                    let escaped = *bytes.get(i).ok_or(ParseError::UnexpectedEof)?;
                    i += 1;
                    let value = match escaped {
                        b'"' | b'\\' => escaped,
                        b'a' => 7,
                        b'b' => 8,
                        b'f' => 12,
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'v' => 11,
                        b'x' => {
                            let mut value = 0_u8;
                            let mut count = 0;
                            while count < 2 && i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                                value = value * 16
                                    + (bytes[i] as char)
                                        .to_digit(16)
                                        .ok_or(ParseError::UnexpectedToken(span.start))?
                                        as u8;
                                i += 1;
                                count += 1;
                            }
                            if count == 0 { b'x' } else { value }
                        }
                        _ => {
                            i -= 1;
                            let mut value = 0_u16;
                            let mut count = 0;
                            while count < 3 && i < bytes.len() && matches!(bytes[i], b'0'..=b'7') {
                                value = value * 8 + u16::from(bytes[i] - b'0');
                                i += 1;
                                count += 1;
                            }
                            if count == 0 { b'\\' } else { value as u8 }
                        }
                    };
                    out.push(value);
                }
                *value =
                    String::from_utf8(out).map_err(|_| ParseError::UnexpectedToken(span.start))?;
            }
            _ => {}
        }
    }
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_escapes_decode_without_changing_source_spans() {
        let source = r#"(x "中文\n\r\t\\\"\x41\101\q\xZ")"#;
        let document = parse_one(source).unwrap();
        let Node::List { items, .. } = &document.nodes[0] else {
            panic!("list")
        };
        let Node::Atom {
            atom: Atom::Quoted(value),
            span,
        } = &items[1]
        else {
            panic!("quoted")
        };
        assert_eq!(value, "中文\n\r\t\\\"AA\\qxZ");
        assert_eq!(&source[span.start..span.end], &source[3..source.len() - 1]);
        assert_eq!(document.raw, source);
        assert!(parse_one("(x \"raw\nnewline\")").is_err());
        assert!(parse_one(r#"(x "\xff")"#).is_err());
    }
}
