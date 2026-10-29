//! Just enough of D-Bus, the desktop's message bus, to show
//! notifications: connecting to the session bus over its Unix socket,
//! the EXTERNAL login (the bus checks the socket's user), writing method
//! calls, and reading the replies and signals that come back.
//!
//! It's a few hundred lines in place of a D-Bus library, as a window needs
//! four calls (`Hello`, `AddMatch`, `Notify`, `CloseNotification`) and
//! three signals, and every library either links the C libdbus or brings
//! an async runtime and dozens of crates. The wire format is the D-Bus
//! specification's: <https://dbus.freedesktop.org/doc/dbus-specification.html>.
//!
//! Everything here blocks, so it only ever runs on the notifier's own
//! threads (see [`super`]), never the window's.

use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// The bus's own name, path and interface, for `Hello` and `AddMatch`,
/// and the sender of its own signals.
pub(super) const BUS: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";

/// The most of a message from the bus that's kept. What a window hears of
/// notifications is a few hundred bytes; a bigger one isn't for it, and
/// is read past, up to what D-Bus allows, 128 MiB.
const MOST: usize = 1 << 20;
const LARGEST: usize = 128 << 20;

/// How long writing to the bus may take before it's given up on.
const WRITE_WAIT: Duration = Duration::from_secs(5);

/// What a message is. An error, 3, is anything that isn't the reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Call = 1,
    Reply = 2,
    Signal = 4,
}

/// A connection to the bus, logged in, for writing calls. What comes back
/// is read from a clone of the socket (see [`Bus::reader`]).
#[derive(Debug)]
pub(super) struct Bus {
    stream: UnixStream,
    /// The last call's number.
    serial: u32,
}

impl Bus {
    /// Connects to the session bus at `address`, a D-Bus address as
    /// `$DBUS_SESSION_BUS_ADDRESS` holds one, and logs in.
    ///
    /// # Errors
    ///
    /// If none of its addresses can be reached, or the bus won't have it.
    pub(super) fn connect(address: &str) -> io::Result<Self> {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "no Unix address to connect to");
        for each in address.split(';') {
            match connect_to(each).and_then(login) {
                Ok(stream) => return Ok(Self { stream, serial: 0 }),
                Err(err) => last = err,
            }
        }
        Err(last)
    }

    /// The socket to read from, a clone of this one's.
    ///
    /// # Errors
    ///
    /// If it can't be cloned.
    pub(super) fn reader(&self) -> io::Result<UnixStream> {
        self.stream.try_clone()
    }

    /// Writes a method call, and returns its number, which its reply
    /// gives back. With `no_reply`, the bus is told none is wanted.
    ///
    /// # Errors
    ///
    /// If it can't be written.
    pub(super) fn call(&mut self, call: &Call<'_>, body: &Body, no_reply: bool) -> io::Result<u32> {
        self.serial = self.serial.wrapping_add(1).max(1);
        let bytes = call.message(self.serial, body, no_reply);
        self.stream.write_all(&bytes)?;
        Ok(self.serial)
    }

    /// Says `Hello`, as the first call on a connection must.
    ///
    /// # Errors
    ///
    /// If it can't be written.
    pub(super) fn hello(&mut self) -> io::Result<u32> {
        let call = Call {
            destination: BUS,
            path: BUS_PATH,
            interface: BUS,
            member: "Hello",
            signature: "",
        };
        self.call(&call, &Body::default(), false)
    }

    /// Asks the bus for the signals `rule` matches.
    ///
    /// # Errors
    ///
    /// If it can't be written.
    pub(super) fn add_match(&mut self, rule: &str) -> io::Result<u32> {
        let call = Call {
            destination: BUS,
            path: BUS_PATH,
            interface: BUS,
            member: "AddMatch",
            signature: "s",
        };
        let mut body = Body::default();
        body.string(rule);
        self.call(&call, &body, true)
    }

    /// Asks the bus which connection owns the name `name`: its reply's
    /// string is its unique name, as `:1.42`, or it's an error if none
    /// does.
    ///
    /// # Errors
    ///
    /// If it can't be written.
    pub(super) fn get_name_owner(&mut self, name: &str) -> io::Result<u32> {
        let call = Call {
            destination: BUS,
            path: BUS_PATH,
            interface: BUS,
            member: "GetNameOwner",
            signature: "s",
        };
        let mut body = Body::default();
        body.string(name);
        self.call(&call, &body, false)
    }
}

impl Drop for Bus {
    /// Ends the connection, so the thread reading it stops too.
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// Connects to `address`, one of a D-Bus address's: `unix:path=…` or
/// `unix:abstract=…`. Others, such as `tcp:`, aren't for a desktop.
fn connect_to(address: &str) -> io::Result<UnixStream> {
    let refused = || {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("can't connect to {address}"),
        )
    };
    let keys = address.strip_prefix("unix:").ok_or_else(refused)?;
    for pair in keys.split(',') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let value = unescape(value).ok_or_else(refused)?;
        match key {
            "path" => {
                use std::os::unix::ffi::OsStrExt as _;
                return UnixStream::connect(std::ffi::OsStr::from_bytes(&value));
            }
            "abstract" => {
                use std::os::linux::net::SocketAddrExt as _;
                let addr = std::os::unix::net::SocketAddr::from_abstract_name(&value)?;
                return UnixStream::connect_addr(&addr);
            }
            _ => {}
        }
    }
    Err(refused())
}

/// An address's value with its `%xx` escapes undone.
fn unescape(value: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            out.push(byte);
        }
    }
    Some(out)
}

/// Logs in on `stream` as its user, with EXTERNAL: the bus checks the
/// socket's credentials against the user id said.
fn login(mut stream: UnixStream) -> io::Result<UnixStream> {
    stream.set_write_timeout(Some(WRITE_WAIT))?;
    stream.set_read_timeout(Some(WRITE_WAIT))?;
    // This process's user, without libc: /proc/self is owned by it.
    let uid = std::fs::metadata("/proc/self")?.uid();
    // In hex: each of its digits, 0x30 to 0x39, is a 3 and the digit.
    let hex: String = (uid.to_string().chars())
        .flat_map(|digit| ['3', digit])
        .collect();
    stream.write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())?;
    let answer = read_line(&mut stream)?;
    if !answer.starts_with("OK ") {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the bus wouldn't log in: {answer}"),
        ));
    }
    stream.write_all(b"BEGIN\r\n")?;
    stream.set_read_timeout(None)?;
    Ok(stream)
}

/// A line of the login, without its `\r\n`. Read a byte at a time, so
/// nothing after it is taken from the socket.
fn read_line(stream: &mut UnixStream) -> io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0];
    while !line.ends_with(b"\r\n") {
        stream.read_exact(&mut byte)?;
        line.push(byte[0]);
        if line.len() > 512 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the bus's line is too long",
            ));
        }
    }
    line.truncate(line.len() - 2);
    Ok(String::from_utf8_lossy(&line).into_owned())
}

/// A method call's addressing.
#[derive(Debug)]
pub(super) struct Call<'a> {
    pub(super) destination: &'a str,
    pub(super) path: &'a str,
    pub(super) interface: &'a str,
    pub(super) member: &'a str,
    /// The body's signature, as `susssasa{sv}i`.
    pub(super) signature: &'a str,
}

impl Call<'_> {
    /// The whole message, numbered `serial`, with `body`.
    fn message(&self, serial: u32, body: &Body, no_reply: bool) -> Vec<u8> {
        let mut header = Body::default();
        header.byte(b'l');
        header.byte(Kind::Call as u8);
        header.byte(u8::from(no_reply));
        header.byte(1);
        header.u32(u32::try_from(body.bytes.len()).unwrap_or(u32::MAX));
        header.u32(serial);
        let fields = header.begin_array(8);
        header.field(1, "o", self.path);
        header.field(2, "s", self.interface);
        header.field(3, "s", self.member);
        header.field(6, "s", self.destination);
        if !self.signature.is_empty() {
            header.align(8);
            header.byte(8);
            header.signature("g");
            header.signature(self.signature);
        }
        header.end_array(fields);
        header.align(8);
        header.bytes.extend_from_slice(&body.bytes);
        header.bytes
    }
}

/// A message's body, or its header, as it's written: each value aligned
/// to its size from the start, as D-Bus wants. A body starts on a multiple
/// of 8 in its message, so aligning from its own start is the same.
#[derive(Debug, Default)]
pub(super) struct Body {
    bytes: Vec<u8>,
}

/// Where an array begins: its length's place, and its first element's.
#[derive(Clone, Copy, Debug)]
pub(super) struct Array {
    length_at: usize,
    start: usize,
}

impl Body {
    fn align(&mut self, to: usize) {
        while !self.bytes.len().is_multiple_of(to) {
            self.bytes.push(0);
        }
    }

    pub(super) fn byte(&mut self, byte: u8) {
        self.bytes.push(byte);
    }

    pub(super) fn u32(&mut self, value: u32) {
        self.align(4);
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(super) fn i32(&mut self, value: i32) {
        self.align(4);
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// A string, or an object path, which is written the same. A NUL in
    /// it, which D-Bus forbids, is dropped.
    pub(super) fn string(&mut self, text: &str) {
        let text: Vec<u8> = text.bytes().filter(|&byte| byte != 0).collect();
        self.u32(u32::try_from(text.len()).unwrap_or(u32::MAX));
        self.bytes.extend_from_slice(&text);
        self.bytes.push(0);
    }

    /// A signature: under 256 bytes, as every one written here is.
    fn signature(&mut self, signature: &str) {
        self.bytes
            .push(u8::try_from(signature.len()).unwrap_or(u8::MAX));
        self.bytes.extend_from_slice(signature.as_bytes());
        self.bytes.push(0);
    }

    /// Begins an array whose elements align to `to`.
    pub(super) fn begin_array(&mut self, to: usize) -> Array {
        self.align(4);
        let length_at = self.bytes.len();
        self.bytes.extend_from_slice(&[0; 4]);
        self.align(to);
        Array {
            length_at,
            start: self.bytes.len(),
        }
    }

    /// Ends `array`, writing its length: its elements', without the
    /// padding before the first.
    pub(super) fn end_array(&mut self, array: Array) {
        let length = u32::try_from(self.bytes.len() - array.start).unwrap_or(u32::MAX);
        self.bytes[array.length_at..array.length_at + 4].copy_from_slice(&length.to_le_bytes());
    }

    /// Begins a struct, or a dictionary's entry, which align to 8.
    pub(super) fn begin_struct(&mut self) {
        self.align(8);
    }

    /// A variant holding the string `text`.
    pub(super) fn variant_string(&mut self, text: &str) {
        self.signature("s");
        self.string(text);
    }

    /// A variant holding the byte `byte`.
    pub(super) fn variant_byte(&mut self, byte: u8) {
        self.signature("y");
        self.byte(byte);
    }

    /// A header field: its code, and a variant of `signature` (`s` or `o`)
    /// holding `text`.
    fn field(&mut self, code: u8, signature: &str, text: &str) {
        self.align(8);
        self.byte(code);
        self.signature(signature);
        self.string(text);
    }

    #[cfg(test)]
    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A message from the bus, as far as a window needs it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Message {
    /// Its kind's number: see [`Kind`].
    pub(super) kind: u8,
    /// The call it answers, for a reply or an error.
    pub(super) reply_serial: Option<u32>,
    pub(super) interface: Option<String>,
    pub(super) member: Option<String>,
    /// The error's name, for an error.
    pub(super) error: Option<String>,
    /// The unique name of the connection that sent it, as the bus says,
    /// which no one else can claim: `:1.42`, or the bus's own name for
    /// its signals.
    pub(super) sender: Option<String>,
    /// The body's signature.
    pub(super) signature: String,
    pub(super) body: Vec<u8>,
    /// Whether its numbers are big-endian, as the sender wrote them.
    pub(super) big: bool,
}

impl Message {
    /// Whether it's `kind`.
    pub(super) fn is(&self, kind: Kind) -> bool {
        self.kind == kind as u8
    }

    /// Whether it's the signal `member` of `interface`.
    pub(super) fn is_signal(&self, interface: &str, member: &str) -> bool {
        self.is(Kind::Signal)
            && self.interface.as_deref() == Some(interface)
            && self.member.as_deref() == Some(member)
    }

    /// Its body, to read values from, if its signature is `signature`.
    pub(super) fn args(&self, signature: &str) -> Option<Cursor<'_>> {
        (self.signature == signature).then(|| Cursor {
            bytes: &self.body,
            at: 0,
            big: self.big,
        })
    }
}

/// Reads the next message from `stream`: `None` for one too big to be for
/// a window, which is read past.
///
/// # Errors
///
/// If it can't be read, isn't a D-Bus message, or is bigger than D-Bus
/// allows.
pub(super) fn read_message(stream: &mut impl Read) -> io::Result<Option<Message>> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let mut fixed = [0u8; 16];
    stream.read_exact(&mut fixed)?;
    let big = match fixed[0] {
        b'l' => false,
        b'B' => true,
        _ => return Err(bad("not a D-Bus message")),
    };
    let number = |at: usize| {
        let bytes = [fixed[at], fixed[at + 1], fixed[at + 2], fixed[at + 3]];
        if big {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        }
    };
    let body_len = number(4) as usize;
    let fields_len = number(12) as usize;
    let header_len = (16 + fields_len).next_multiple_of(8);
    let whole_len = header_len.saturating_add(body_len);
    if whole_len > LARGEST {
        return Err(bad("a message from the bus is bigger than D-Bus allows"));
    }
    if whole_len > MOST {
        let rest = (whole_len - 16) as u64;
        if io::copy(&mut stream.by_ref().take(rest), &mut io::sink())? < rest {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        return Ok(None);
    }
    let mut rest = vec![0; whole_len - 16];
    stream.read_exact(&mut rest)?;
    let mut whole = fixed.to_vec();
    whole.extend_from_slice(&rest);
    parse(&whole, big, fields_len, header_len)
        .map(Some)
        .ok_or_else(|| bad("a message from the bus is malformed"))
}

/// Reads `whole`, a message, whose header fields take `fields_len` bytes
/// after the first 16, and whose header ends at `header_len`.
fn parse(whole: &[u8], big: bool, fields_len: usize, header_len: usize) -> Option<Message> {
    let mut message = Message {
        kind: whole[1],
        big,
        body: whole[header_len..].to_vec(),
        ..Message::default()
    };
    let mut fields = Cursor {
        bytes: &whole[..16 + fields_len],
        at: 16,
        big,
    };
    while fields.at < fields.bytes.len() {
        fields.align(8)?;
        let code = fields.byte()?;
        let signature = fields.signature()?;
        match signature {
            "s" | "o" => {
                let text = fields.string()?.to_owned();
                match code {
                    2 => message.interface = Some(text),
                    3 => message.member = Some(text),
                    4 => message.error = Some(text),
                    7 => message.sender = Some(text),
                    _ => {}
                }
            }
            "g" => {
                let text = fields.signature()?.to_owned();
                if code == 8 {
                    message.signature = text;
                }
            }
            "u" => {
                let value = fields.u32()?;
                if code == 5 {
                    message.reply_serial = Some(value);
                }
            }
            // No other type is a header field's.
            _ => return None,
        }
    }
    Some(message)
}

/// Where reading a message's values has got to.
#[derive(Debug)]
pub(super) struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
    big: bool,
}

impl<'a> Cursor<'a> {
    fn align(&mut self, to: usize) -> Option<()> {
        self.at = self.at.next_multiple_of(to);
        (self.at <= self.bytes.len()).then_some(())
    }

    fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    pub(super) fn u32(&mut self) -> Option<u32> {
        self.align(4)?;
        let bytes: [u8; 4] = self.bytes.get(self.at..self.at + 4)?.try_into().ok()?;
        self.at += 4;
        Some(if self.big {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    }

    pub(super) fn string(&mut self) -> Option<&'a str> {
        let len = self.u32()? as usize;
        let text = self.bytes.get(self.at..self.at.checked_add(len)?)?;
        self.at += len + 1;
        std::str::from_utf8(text).ok()
    }

    fn signature(&mut self) -> Option<&'a str> {
        let len = usize::from(self.byte()?);
        let text = self.bytes.get(self.at..self.at + len)?;
        self.at += len + 1;
        std::str::from_utf8(text).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The call written to close notification 7, byte for byte, as the
    /// specification lays a message out.
    #[test]
    fn a_call_is_laid_out_as_the_specification_says() {
        let call = Call {
            destination: "a.b",
            path: "/p",
            interface: "a.b",
            member: "M",
            signature: "u",
        };
        let mut body = Body::default();
        body.u32(7);
        let bytes = call.message(3, &body, true);
        let mut want: Vec<u8> = vec![b'l', 1, 1, 1, 4, 0, 0, 0, 3, 0, 0, 0];
        // The fields' length: 4 fields of 16 bytes with their padding, and
        // the signature's 7, without the padding after it.
        want.extend_from_slice(&[16 * 4 + 7, 0, 0, 0]);
        // PATH, an object path "/p".
        want.extend_from_slice(&[1, 1, b'o', 0, 2, 0, 0, 0, b'/', b'p', 0, 0, 0, 0, 0, 0]);
        // INTERFACE, a string "a.b".
        want.extend_from_slice(&[2, 1, b's', 0, 3, 0, 0, 0, b'a', b'.', b'b', 0, 0, 0, 0, 0]);
        // MEMBER, "M".
        want.extend_from_slice(&[3, 1, b's', 0, 1, 0, 0, 0, b'M', 0, 0, 0, 0, 0, 0, 0]);
        // DESTINATION, "a.b".
        want.extend_from_slice(&[6, 1, b's', 0, 3, 0, 0, 0, b'a', b'.', b'b', 0, 0, 0, 0, 0]);
        // SIGNATURE, "u", then padding to 8 before the body.
        want.extend_from_slice(&[8, 1, b'g', 0, 1, b'u', 0, 0]);
        want.extend_from_slice(&[7, 0, 0, 0]);
        assert_eq!(bytes, want);
    }

    #[test]
    fn arrays_count_their_elements_not_the_padding_before_them() {
        let mut body = Body::default();
        body.byte(1);
        let array = body.begin_array(8);
        body.begin_struct();
        body.string("k");
        body.variant_byte(2);
        body.end_array(array);
        // A byte, padding to 4, the length, padding to 8, then the entry:
        // "k" (4 + 2 bytes), the signature "y" (3) and the byte.
        assert_eq!(&body.as_bytes()[4..8], &[10, 0, 0, 0]);
        assert_eq!(body.as_bytes().len(), 8 + 10);
    }

    /// A message as the bus would send one, in either byte order.
    fn signal(big: bool, member: &str, signature: &str, body: &[u8]) -> Vec<u8> {
        let number = |value: u32| {
            if big {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            }
        };
        let mut fields = Vec::new();
        let mut field = |code: u8, sig: u8, text: &str| {
            while fields.len() % 8 != 0 {
                fields.push(0);
            }
            fields.extend_from_slice(&[code, 1, sig, 0]);
            if sig == b'g' {
                fields.push(u8::try_from(text.len()).unwrap());
            } else {
                fields.extend_from_slice(&number(u32::try_from(text.len()).unwrap()));
            }
            fields.extend_from_slice(text.as_bytes());
            fields.push(0);
        };
        field(1, b'o', "/org/freedesktop/Notifications");
        field(2, b's', "org.freedesktop.Notifications");
        field(3, b's', member);
        field(7, b's', ":1.9");
        field(8, b'g', signature);
        let mut message = vec![if big { b'B' } else { b'l' }, 4, 1, 1];
        message.extend_from_slice(&number(u32::try_from(body.len()).unwrap()));
        message.extend_from_slice(&number(9));
        message.extend_from_slice(&number(u32::try_from(fields.len()).unwrap()));
        message.extend_from_slice(&fields);
        while message.len() % 8 != 0 {
            message.push(0);
        }
        message.extend_from_slice(body);
        message
    }

    #[test]
    fn signals_are_read_in_either_byte_order() {
        for big in [false, true] {
            let number = |value: u32| {
                if big {
                    value.to_be_bytes()
                } else {
                    value.to_le_bytes()
                }
            };
            let mut body = number(42).to_vec();
            body.extend_from_slice(&number(7));
            body.extend_from_slice(b"default\0");
            let bytes = signal(big, "ActionInvoked", "us", &body);
            let message = read_message(&mut bytes.as_slice()).unwrap().unwrap();
            assert!(message.is_signal("org.freedesktop.Notifications", "ActionInvoked"));
            assert_eq!(message.sender.as_deref(), Some(":1.9"));
            let mut args = message.args("us").unwrap();
            assert_eq!(args.u32(), Some(42));
            assert_eq!(args.string(), Some("default"));
            assert!(message.args("uu").is_none());
        }
    }

    #[test]
    fn what_isnt_a_message_or_is_too_big_is_refused() {
        assert!(read_message(&mut &[b'x'; 16][..]).is_err());
        let mut huge = signal(false, "M", "", &[]);
        huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_message(&mut huge.as_slice()).is_err());
        // Cut short.
        let whole = signal(false, "M", "", &[]);
        assert!(read_message(&mut &whole[..whole.len() - 3]).is_err());
    }

    #[test]
    fn a_message_too_big_for_a_window_is_read_past_not_fatal() {
        let mut stream = signal(false, "Big", "ay", &vec![7; 2 << 20]);
        stream.extend_from_slice(&signal(false, "Next", "", &[]));
        let mut reading = stream.as_slice();
        assert_eq!(read_message(&mut reading).unwrap(), None);
        let next = read_message(&mut reading).unwrap().unwrap();
        assert_eq!(next.member.as_deref(), Some("Next"));
    }

    #[test]
    fn a_truncated_string_in_a_body_reads_as_none() {
        let message = Message {
            signature: "s".to_owned(),
            body: vec![200, 0, 0, 0, b'a'],
            ..Message::default()
        };
        assert_eq!(message.args("s").unwrap().string(), None);
    }

    #[test]
    fn addresses_are_unescaped_and_only_unix_ones_are_tried() {
        assert_eq!(
            unescape("/run/user/1000/b%2cus").unwrap(),
            b"/run/user/1000/b,us"
        );
        assert!(unescape("%z").is_none());
        assert!(connect_to("tcp:host=localhost,port=1").is_err());
    }

    /// The whole round: a bus that logs the window in and answers
    /// `Hello`, on a socket pair, with no real bus.
    #[test]
    fn logging_in_says_external_with_the_user_and_begins() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let bus = std::thread::spawn(move || {
            let mut said = Vec::new();
            let mut byte = [0];
            while !said.ends_with(b"\r\n") {
                theirs.read_exact(&mut byte).unwrap();
                said.push(byte[0]);
            }
            theirs.write_all(b"OK 0123456789abcdef\r\n").unwrap();
            let mut begin = [0; 7];
            theirs.read_exact(&mut begin).unwrap();
            (String::from_utf8(said).unwrap(), begin)
        });
        login(ours).unwrap();
        let (said, begin) = bus.join().unwrap();
        let uid = std::fs::metadata("/proc/self").unwrap().uid();
        let hex: String = (uid.to_string().bytes())
            .map(|byte| format!("{byte:02x}"))
            .fold(String::new(), |hex, byte| hex + &byte);
        assert_eq!(said, format!("\0AUTH EXTERNAL {hex}\r\n"));
        assert_eq!(&begin, b"BEGIN\r\n");
    }
}
