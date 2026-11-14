//! JSON-RPC 2.0 messages over a byte stream, framed by a `Content-Length` header as the language
//! server protocol frames them.

use std::io::{self, BufRead, Read, Write};

use serde_json::Value;

/// The largest message body read, so a server cannot make the client allocate without bound.
pub(super) const MESSAGE_MAX: usize = 16 << 20;
/// The longest header line read, its line end included.
const HEADER_LINE_MAX: u64 = 1024;

/// Writes `message` with its header and flushes.
pub(super) fn write(out: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(&body)?;
    out.flush()
}

/// The next message, or `None` when the stream ends before one starts. Headers other than
/// `Content-Length` are ignored; a body larger than [`MESSAGE_MAX`] is an error.
pub(super) fn read(input: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    let mut started = false;
    loop {
        let mut line = Vec::new();
        (input.by_ref().take(HEADER_LINE_MAX)).read_until(b'\n', &mut line)?;
        if line.is_empty() && !started {
            return Ok(None);
        }
        if line.last() != Some(&b'\n') {
            return Err(invalid(
                "a header line is too long or the stream ended inside it",
            ));
        }
        started = true;
        let line = std::str::from_utf8(&line).map_err(|_| invalid("a header is not UTF-8"))?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            let value = value.trim().parse::<usize>();
            length = Some(value.map_err(|_| invalid("Content-Length is not a number"))?);
        }
    }
    let length = length.ok_or_else(|| invalid("a message has no Content-Length"))?;
    if length > MESSAGE_MAX {
        let message = format!("a message of {length} bytes is over the {MESSAGE_MAX} byte cap");
        return Err(invalid(message));
    }
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body).map_err(|e| invalid(format!("bad JSON: {e}")))?;
    Ok(Some(value))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::BufReader;

    /// Yields one byte per read, as a slow pipe might.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some((&first, rest)) = self.0.split_first() else {
                return Ok(0);
            };
            if buf.is_empty() {
                return Ok(0);
            }
            buf[0] = first;
            self.0 = rest;
            Ok(1)
        }
    }

    #[test]
    fn messages_round_trip_through_partial_reads() {
        let (a, b) = (json!({"id": 1, "result": "é"}), json!({"method": "exit"}));
        let mut bytes = Vec::new();
        write(&mut bytes, &a).unwrap();
        write(&mut bytes, &b).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.starts_with("Content-Length: 22\r\n\r\n{"), "{text}");

        let mut input = BufReader::with_capacity(1, Trickle(&bytes));
        assert_eq!(read(&mut input).unwrap(), Some(a));
        assert_eq!(read(&mut input).unwrap(), Some(b));
        assert_eq!(read(&mut input).unwrap(), None);
    }

    #[test]
    fn other_headers_are_ignored_and_bad_frames_are_errors() {
        let framed = b"content-length: 2\r\nContent-Type: x\r\n\r\n{}";
        assert_eq!(read(&mut &framed[..]).unwrap(), Some(json!({})));

        let over = format!("Content-Length: {}\r\n\r\n", MESSAGE_MAX + 1);
        let error = read(&mut over.as_bytes()).unwrap_err();
        assert!(error.to_string().contains("byte cap"), "{error}");

        for bad in [
            &b"Content-Type: x\r\n\r\n{}"[..],
            b"Content-Length: 9\r\n\r\n{}",
            b"Content-Length: 2\r\n",
            b"Content-Length: x\r\n\r\n{}",
            b"Content-Length: 1\r\n\r\n{",
        ] {
            assert!(
                read(&mut &bad[..]).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        let long = format!("X: {}\r\n\r\n", "a".repeat(2000));
        assert!(read(&mut long.as_bytes()).is_err());
    }
}
