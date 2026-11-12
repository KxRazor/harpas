//! A connection's socket, read on a thread of its own so that the connection's thread can wait for the client
//! and for the logic thread at once.
//!
//! The connection's thread parks while it has nothing to do. The reader unparks it when bytes arrive, and the
//! logic thread when it queues a message for the player (`harpas_engine::Handle::player_joined_with_view`), so
//! neither waits for a timer.

use std::io::{self, Read, Write};
use std::net::{self, TcpStream};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::{self, Thread};
use std::time::Duration;

/// Largest read from the socket.
const READ_SIZE: usize = 16 * 1024;
/// Reads the connection's thread has not taken yet before the reader stops reading, which leaves the rest to
/// TCP's flow control.
const QUEUED_READS: usize = 4;

/// A stream whose reads return [`io::ErrorKind::WouldBlock`] when the thread is unparked, or after `idle`
/// without bytes from the client.
pub(crate) struct Socket {
    stream: TcpStream,
    received: Receiver<io::Result<Vec<u8>>>,
    /// Received bytes the last read had no room for.
    rest: Vec<u8>,
    idle: Duration,
}

impl Socket {
    /// Starts reading `stream` for the calling thread, which must be the one that reads the socket. A write
    /// that makes no progress for `write_timeout` fails.
    pub(crate) fn new(
        stream: TcpStream,
        write_timeout: Duration,
        idle: Duration,
    ) -> io::Result<Self> {
        stream.set_write_timeout(Some(write_timeout))?;
        stream.set_nodelay(true)?;
        let reading = stream.try_clone()?;
        let (sender, received) = mpsc::sync_channel(QUEUED_READS);
        let connection = thread::current();
        let name = match stream.peer_addr() {
            Ok(peer) => format!("Reader {peer}"),
            Err(_) => "Reader".to_owned(),
        };
        thread::Builder::new()
            .name(name)
            // The reader only copies bytes; its buffer is on the heap.
            .stack_size(64 * 1024)
            .spawn(move || read(reading, &sender, &connection))?;
        Ok(Self {
            stream,
            received,
            rest: Vec::new(),
            idle,
        })
    }
}

/// Forwards what `stream` receives until it ends, fails or the connection's thread drops its [`Socket`]. The
/// end of the stream is an empty read.
fn read(mut stream: TcpStream, sender: &SyncSender<io::Result<Vec<u8>>>, connection: &Thread) {
    let mut buf = vec![0; READ_SIZE];
    loop {
        let read = match stream.read(&mut buf) {
            Ok(read) => Ok(buf[..read].to_vec()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => Err(e),
        };
        let last = read.as_ref().map_or(true, Vec::is_empty);
        if sender.send(read).is_err() {
            return;
        }
        connection.unpark();
        if last {
            return;
        }
    }
}

impl Read for Socket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.rest.is_empty() {
            let mut received = self.received.try_recv();
            if matches!(received, Err(TryRecvError::Empty)) {
                // Returns at once if the thread was unparked since it last parked.
                thread::park_timeout(self.idle);
                received = self.received.try_recv();
            }
            match received {
                Ok(Ok(bytes)) => self.rest = bytes,
                Ok(Err(e)) => return Err(e),
                Err(TryRecvError::Empty) => return Err(io::ErrorKind::WouldBlock.into()),
                Err(TryRecvError::Disconnected) => {}
            }
        }
        let read = self.rest.len().min(buf.len());
        buf[..read].copy_from_slice(&self.rest[..read]);
        self.rest.drain(..read);
        Ok(read)
    }
}

impl Write for Socket {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        // Ends the reader's read too.
        let _ = self.stream.shutdown(net::Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::time::Instant;

    use super::*;

    /// Longer than any test waits: a read that returns sooner was woken.
    const IDLE: Duration = Duration::from_secs(30);
    const PROMPT: Duration = Duration::from_secs(10);

    fn pair(idle: Duration) -> (Socket, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let socket = Socket::new(stream, Duration::from_secs(30), idle).unwrap();
        (socket, client)
    }

    /// Reads until bytes arrive: a thread may be unparked for other reasons.
    fn read_bytes(socket: &mut Socket, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match socket.read(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                result => return result,
            }
        }
    }

    #[test]
    fn bytes_from_the_client_wake_the_read() {
        let (mut socket, mut client) = pair(IDLE);
        let started = Instant::now();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            client.write_all(b"hello").unwrap();
            client
        });
        let mut buf = [0; 16];
        let read = read_bytes(&mut socket, &mut buf).unwrap();
        assert_eq!(&buf[..read], b"hello");
        assert!(started.elapsed() < PROMPT);
        drop(writer.join().unwrap());
        // The end of the stream wakes it too.
        assert_eq!(read_bytes(&mut socket, &mut buf).unwrap(), 0);
        assert!(started.elapsed() < PROMPT);
    }

    #[test]
    fn unparking_the_thread_ends_the_read() {
        let (mut socket, _client) = pair(IDLE);
        let connection = thread::current();
        let started = Instant::now();
        let waker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            connection.unpark();
        });
        let error = socket.read(&mut [0; 16]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(started.elapsed() < PROMPT);
        waker.join().unwrap();
    }

    #[test]
    fn an_idle_read_gives_up_after_the_interval() {
        let (mut socket, _client) = pair(Duration::from_millis(50));
        let error = socket.read(&mut [0; 16]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn a_read_larger_than_the_buffer_is_kept() {
        let (mut socket, mut client) = pair(IDLE);
        client.write_all(b"hello world").unwrap();
        let mut received = Vec::new();
        let mut buf = [0; 4];
        while received.len() < 11 {
            let read = read_bytes(&mut socket, &mut buf).unwrap();
            received.extend_from_slice(&buf[..read]);
        }
        assert_eq!(received, b"hello world");
    }

    #[test]
    fn dropping_the_socket_closes_the_connection() {
        let (socket, mut client) = pair(IDLE);
        drop(socket);
        assert_eq!(client.read(&mut [0; 16]).unwrap(), 0);
    }
}
