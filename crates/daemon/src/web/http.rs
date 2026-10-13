//! Just enough HTTP/1.1 for the web app: requests read with `httparse`,
//! and answers written whole.
//!
//! A request's head is at most [`MAX_HEAD`] bytes, and its body, sent
//! with a `Content-Length`, at most [`MAX_BODY`]: the app's requests are
//! small, and anything bigger is refused before it's buffered. Chunked
//! bodies aren't taken at all.

use std::borrow::Cow;

/// The most bytes a request's head may take.
pub(super) const MAX_HEAD: usize = 8 * 1024;

/// The most bytes a request's body may take.
pub(super) const MAX_BODY: usize = 8 * 1024;

/// The most headers a request may have.
const MAX_HEADERS: usize = 40;

/// What every page is allowed to do: load the app's own script, styles
/// and WebAssembly, and talk to the daemon, and nothing else. No inline
/// scripts or styles, no frames, no other origins.
pub(super) const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; \
     script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' data:; \
     connect-src 'self'; manifest-src 'self'; worker-src 'self'; base-uri 'none'; \
     form-action 'none'; frame-ancestors 'none'";

/// A request's method, as far as the app uses them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Head,
    Post,
    Other,
}

/// A request, read whole.
#[derive(Debug)]
pub(crate) struct Request {
    pub(crate) method: Method,
    /// The path, without the query.
    pub(super) path: String,
    /// The headers, their names in lowercase.
    headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
}

impl Request {
    /// The value of the header called `name`, in lowercase, if it was sent
    /// once. A header sent twice counts as not sent, so it can't say two
    /// things to two readers.
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        let mut values = (self.headers.iter())
            .filter(|(header, _)| header == name)
            .map(|(_, value)| value.as_str());
        let value = values.next()?;
        values.next().is_none().then_some(value)
    }

    /// Whether the header called `name` lists `token`, ignoring case, as
    /// `Connection: keep-alive, Upgrade` lists `upgrade`.
    pub(super) fn lists(&self, name: &str, token: &str) -> bool {
        (self.headers.iter())
            .filter(|(header, _)| header == name)
            .flat_map(|(_, value)| value.split(','))
            .any(|item| item.trim().eq_ignore_ascii_case(token))
    }

    /// The value of the cookie called `name`, if it was sent.
    pub(super) fn cookie(&self, name: &str) -> Option<&str> {
        (self.headers.iter())
            .filter(|(header, _)| header == "cookie")
            .flat_map(|(_, value)| value.split(';'))
            .find_map(|pair| {
                let (key, value) = pair.trim().split_once('=')?;
                (key == name).then_some(value)
            })
    }

    /// Whether the connection is to close once this is answered: HTTP/1.1
    /// keeps it open unless asked not to.
    pub(super) fn closes(&self) -> bool {
        self.lists("connection", "close")
    }
}

/// What's come of the bytes read so far.
#[derive(Debug)]
pub(super) enum Parsed {
    /// Not a whole request yet.
    Partial,
    /// A request, and how many bytes it took.
    Done(Request, usize),
    /// Not a request that's taken, with the status to answer.
    Bad(u16),
}

/// Reads a request from the start of `input`.
pub(super) fn parse(input: &[u8]) -> Parsed {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);
    let head = match request.parse(input) {
        Ok(httparse::Status::Complete(head)) => head,
        Ok(httparse::Status::Partial) if input.len() > MAX_HEAD => return Parsed::Bad(431),
        Ok(httparse::Status::Partial) => return Parsed::Partial,
        Err(httparse::Error::TooManyHeaders) => return Parsed::Bad(431),
        Err(_) => return Parsed::Bad(400),
    };
    if head > MAX_HEAD {
        return Parsed::Bad(431);
    }
    let method = match request.method {
        Some("GET") => Method::Get,
        Some("HEAD") => Method::Head,
        Some("POST") => Method::Post,
        _ => Method::Other,
    };
    let path = request.path.unwrap_or("/");
    let path = path
        .split_once('?')
        .map_or(path, |(path, _)| path)
        .to_owned();
    let mut parsed = Vec::with_capacity(request.headers.len());
    for header in request.headers.iter() {
        let Ok(value) = std::str::from_utf8(header.value) else {
            return Parsed::Bad(400);
        };
        parsed.push((header.name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    let mut request = Request {
        method,
        path,
        headers: parsed,
        body: Vec::new(),
    };
    if request
        .headers
        .iter()
        .any(|(name, _)| name == "transfer-encoding")
    {
        return Parsed::Bad(411);
    }
    let length = match request.header("content-length").map(str::parse::<usize>) {
        None if request
            .headers
            .iter()
            .any(|(name, _)| name == "content-length") =>
        {
            return Parsed::Bad(400);
        }
        None => 0,
        Some(Ok(length)) if length <= MAX_BODY => length,
        Some(Ok(_)) => return Parsed::Bad(413),
        Some(Err(_)) => return Parsed::Bad(400),
    };
    if input.len() < head + length {
        return Parsed::Partial;
    }
    request.body = input[head..head + length].to_vec();
    Parsed::Done(request, head + length)
}

/// An answer to a request.
#[derive(Debug)]
pub(crate) struct Response {
    pub(super) status: u16,
    headers: Vec<(&'static str, String)>,
    body: Cow<'static, [u8]>,
}

impl Response {
    /// An answer with `status` and no body.
    pub(super) fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Cow::Borrowed(&[]),
        }
    }

    /// An answer with `status` and `body`, of type `content_type`.
    pub(super) fn new(
        status: u16,
        content_type: &str,
        body: impl Into<Cow<'static, [u8]>>,
    ) -> Self {
        Self::empty(status)
            .with("content-type", content_type)
            .body(body)
    }

    /// An answer with `status` and `value` as JSON, which the app's
    /// own requests are answered with. Never kept by a cache.
    pub(super) fn json(status: u16, value: &serde_json::Value) -> Self {
        Self::new(status, "application/json", value.to_string().into_bytes())
            .with("cache-control", "no-store")
    }

    /// An answer with `status` saying `error`, as JSON.
    pub(super) fn error(status: u16, error: &str) -> Self {
        Self::json(status, &serde_json::json!({ "error": error }))
    }

    /// The same answer with the header `name` set to `value`.
    pub(super) fn with(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    /// The same answer with `body`.
    pub(super) fn body(mut self, body: impl Into<Cow<'static, [u8]>>) -> Self {
        self.body = body.into();
        self
    }

    /// Writes it to `out`, leaving out the body if `head_only`, as for a
    /// HEAD request, and with the headers every answer has: those keeping
    /// pages from being framed, sniffed or given away in referrers, and
    /// what pages may do, and if `secure`, that the app is only to be
    /// reached over HTTPS.
    pub(super) fn write(&self, out: &mut Vec<u8>, head_only: bool, secure: bool) {
        use std::io::Write as _;
        let _ = write!(out, "HTTP/1.1 {} {}\r\n", self.status, reason(self.status));
        for (name, value) in &self.headers {
            let _ = write!(out, "{name}: {value}\r\n");
        }
        // An answer that can't have a body says nothing of its length.
        if !matches!(self.status, 101 | 204 | 304) {
            let _ = write!(out, "content-length: {}\r\n", self.body.len());
        }
        let _ = write!(
            out,
            "x-content-type-options: nosniff\r\nreferrer-policy: no-referrer\r\n\
             x-frame-options: DENY\r\ncontent-security-policy: {CONTENT_SECURITY_POLICY}\r\n",
        );
        if secure {
            out.extend_from_slice(b"strict-transport-security: max-age=31536000\r\n");
        }
        out.extend_from_slice(b"\r\n");
        if !head_only {
            out.extend_from_slice(&self.body);
        }
    }
}

/// The words that go with `status`.
fn reason(status: u16) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        426 => "Upgrade Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(input: &[u8]) -> (Request, usize) {
        match parse(input) {
            Parsed::Done(request, used) => (request, used),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_request_is_read_once_its_body_is_all_there() {
        let input = b"POST /pair?x=1 HTTP/1.1\r\nHost: a\r\nContent-Length: 4\r\n\
                      Cookie: a=b; __Host-c=tok\r\nConnection: keep-alive, Upgrade\r\n\r\nbody";
        for cut in 0..input.len() {
            assert!(matches!(parse(&input[..cut]), Parsed::Partial), "at {cut}");
        }
        let (request, used) = done(input);
        assert_eq!(used, input.len());
        assert_eq!(request.method, Method::Post);
        assert_eq!(request.path, "/pair");
        assert_eq!(request.header("host"), Some("a"));
        assert_eq!(request.cookie("__Host-c"), Some("tok"));
        assert_eq!(request.cookie("c"), None);
        assert!(request.lists("connection", "upgrade"));
        assert!(!request.closes());
        assert_eq!(request.body, b"body");
    }

    #[test]
    fn what_isnt_taken_is_refused_before_its_buffered() {
        let status = |input: &[u8]| match parse(input) {
            Parsed::Bad(status) => status,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            status(b"GET / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            411
        );
        let big = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert_eq!(status(big.as_bytes()), 413);
        assert_eq!(status(b"GET / HTTP/1.1\r\nContent-Length: x\r\n\r\n"), 400);
        let long = format!("GET / HTTP/1.1\r\nX: {}", "a".repeat(MAX_HEAD));
        assert_eq!(status(long.as_bytes()), 431);
        assert_eq!(status(b"\x00\x01 nonsense\r\n\r\n"), 400);
    }

    #[test]
    fn a_header_sent_twice_says_nothing() {
        let (request, _) =
            done(b"GET / HTTP/1.1\r\nOrigin: https://a\r\nOrigin: https://b\r\n\r\n");
        assert_eq!(request.header("origin"), None);
    }

    #[test]
    fn answers_carry_what_keeps_pages_safe() {
        let mut out = Vec::new();
        Response::new(200, "text/plain", &b"hi"[..]).write(&mut out, false, true);
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("content-length: 2\r\n"));
        assert!(text.contains("frame-ancestors 'none'"));
        assert!(text.contains("strict-transport-security"));
        assert!(text.ends_with("\r\n\r\nhi"));
        let mut out = Vec::new();
        Response::empty(101).write(&mut out, false, false);
        assert!(!String::from_utf8(out).unwrap().contains("content-length"));
    }
}
