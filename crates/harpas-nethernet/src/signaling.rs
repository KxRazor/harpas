//! The HTTP signaling endpoint clients use to join by address: `GET /v1/join` and `POST /v1/join/{networkID}`
//!
//! A small HTTP/1.1 subset: one request per connection, a `Content-Length` body, no chunked encoding. The parser
//! does no I/O and checks every size before buffering more.

use std::fmt;

/// Largest request head: request line and headers.
pub const MAX_HEAD: usize = 8 * 1024;
/// Largest request body. An SDP offer is a few kilobytes.
pub const MAX_BODY: usize = 64 * 1024;

/// What a signaling request asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// `GET /v1/join`: the server list entry.
    Status,
    /// `POST /v1/join/{networkID}`: an SDP offer to answer.
    Offer {
        /// The client's network ID.
        network_id: u64,
        /// The SDP offer.
        sdp: String,
    },
}

/// Why a request was refused, with the HTTP status to answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 400: the request is not valid HTTP or misses a field.
    BadRequest(&'static str),
    /// 404: an unknown path.
    NotFound,
    /// 405: a known path with another method.
    MethodNotAllowed,
    /// 413: the head or the body is too large.
    TooLarge,
}

impl Error {
    /// The HTTP status code and reason phrase.
    pub fn status(&self) -> (u16, &'static str) {
        match self {
            Error::BadRequest(_) => (400, "Bad Request"),
            Error::NotFound => (404, "Not Found"),
            Error::MethodNotAllowed => (405, "Method Not Allowed"),
            Error::TooLarge => (413, "Content Too Large"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadRequest(why) => write!(f, "bad request: {why}"),
            other => f.write_str(other.status().1),
        }
    }
}

impl std::error::Error for Error {}

/// Parses a request from the bytes received so far: `Ok(None)` while more bytes are needed.
pub fn parse(input: &[u8]) -> Result<Option<Request>, Error> {
    let Some(head_end) = input.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if input.len() > MAX_HEAD {
            Err(Error::TooLarge)
        } else {
            Ok(None)
        };
    };
    if head_end > MAX_HEAD {
        return Err(Error::TooLarge);
    }
    let head = std::str::from_utf8(&input[..head_end])
        .map_err(|_| Error::BadRequest("head is not UTF-8"))?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let (method, target, version) = (
        request_line.next(),
        request_line.next(),
        request_line.next(),
    );
    let (Some(method), Some(target), Some(version)) = (method, target, version) else {
        return Err(Error::BadRequest("malformed request line"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(Error::BadRequest("unsupported HTTP version"));
    }
    let mut content_length = 0;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or(Error::BadRequest("malformed header"))?;
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value
                .trim()
                .parse()
                .map_err(|_| Error::BadRequest("invalid Content-Length"))?;
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Error::BadRequest("transfer encodings are not supported"));
        }
    }
    if content_length > MAX_BODY {
        return Err(Error::TooLarge);
    }
    let body_start = head_end + 4;
    if input.len() < body_start + content_length {
        return Ok(None);
    }
    let body = &input[body_start..body_start + content_length];
    // Query strings are ignored.
    let path = target.split('?').next().unwrap_or_default();
    match (method, path.strip_prefix("/v1/join")) {
        ("GET", Some("" | "/")) => Ok(Some(Request::Status)),
        ("POST", Some(rest)) if rest.starts_with('/') && rest.len() > 1 => {
            let network_id = rest[1..]
                .parse()
                .map_err(|_| Error::BadRequest("network ID must be a u64"))?;
            let sdp = String::from_utf8(body.to_vec())
                .map_err(|_| Error::BadRequest("offer is not UTF-8"))?;
            if sdp.is_empty() {
                return Err(Error::BadRequest("missing SDP offer"));
            }
            Ok(Some(Request::Offer { network_id, sdp }))
        }
        (_, Some(_)) => Err(Error::MethodNotAllowed),
        _ => Err(Error::NotFound),
    }
}

/// An HTTP response; every connection closes after it.
pub fn response(status: u16, reason: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// The response for a refused request.
pub fn error_response(error: &Error) -> Vec<u8> {
    let (status, reason) = error.status();
    response(status, reason, "text/plain", error.to_string().as_bytes())
}

/// The server list entry answered to `GET /v1/join`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Server name, the first line of the entry.
    pub name: String,
    /// Bedrock protocol number.
    pub protocol: u32,
    /// Bedrock version name.
    pub version: String,
    /// World name.
    pub level: String,
    /// Players online.
    pub players: u32,
    /// Player limit.
    pub max_players: u32,
    /// Game mode: 0 survival, 1 creative, 2 adventure.
    pub game_type: u8,
}

impl Status {
    /// The JSON body.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"name":{},"protocol":{},"version":{},"level":{},"players":{},"maxPlayers":{},"gameType":{}}}"#,
            json_string(&self.name),
            self.protocol,
            json_string(&self.version),
            json_string(&self.level),
            self.players,
            self.max_players,
            self.game_type,
        )
    }
}

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_two_requests() {
        let status = b"GET /v1/join HTTP/1.1\r\nHost: 192.168.1.2:19132\r\n\r\n";
        assert_eq!(parse(status), Ok(Some(Request::Status)));
        let offer = b"POST /v1/join/1234 HTTP/1.1\r\nContent-Length: 5\r\nContent-Type: application/sdp\r\n\r\nv=0\r\n";
        assert_eq!(
            parse(offer),
            Ok(Some(Request::Offer {
                network_id: 1234,
                sdp: "v=0\r\n".into()
            }))
        );
        // Incomplete head, then incomplete body.
        assert_eq!(parse(&offer[..20]), Ok(None));
        assert_eq!(parse(&offer[..offer.len() - 1]), Ok(None));
    }

    #[test]
    fn refuses_bad_requests() {
        assert_eq!(parse(b"GET /other HTTP/1.1\r\n\r\n"), Err(Error::NotFound));
        assert_eq!(
            parse(b"DELETE /v1/join HTTP/1.1\r\n\r\n"),
            Err(Error::MethodNotAllowed)
        );
        assert_eq!(
            parse(b"POST /v1/join/abc HTTP/1.1\r\nContent-Length: 1\r\n\r\nx"),
            Err(Error::BadRequest("network ID must be a u64"))
        );
        assert_eq!(
            parse(b"POST /v1/join/1 HTTP/1.1\r\nContent-Length: 999999\r\n\r\n"),
            Err(Error::TooLarge)
        );
        assert_eq!(parse(&vec![b'a'; MAX_HEAD + 1]), Err(Error::TooLarge));
        assert_eq!(
            parse(b"POST /v1/join/1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Err(Error::BadRequest("transfer encodings are not supported"))
        );
    }

    #[test]
    fn status_is_the_json_clients_read() {
        let status = Status {
            name: "Harpas \"test\"".into(),
            protocol: 2225,
            version: "1.26.60".into(),
            level: "world".into(),
            players: 0,
            max_players: 10,
            game_type: 0,
        };
        assert_eq!(
            status.to_json(),
            r#"{"name":"Harpas \"test\"","protocol":2225,"version":"1.26.60","level":"world","players":0,"maxPlayers":10,"gameType":0}"#
        );
    }
}
