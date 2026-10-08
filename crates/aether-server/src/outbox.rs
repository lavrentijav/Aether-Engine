//! A connection's outgoing packets: a bounded queue and a writer thread.
//!
//! Every thread that talks to a player — the player's own, the game tick,
//! another player's broadcast — used to write straight into the socket under
//! a mutex. A client that stopped reading (a server scanner that connected
//! and went quiet, a frozen client) then filled the kernel buffer, the write
//! blocked forever with the mutex held, and the next thread to talk to that
//! player blocked behind it: the tick first, then every connection. One silent
//! peer stopped the whole server.
//!
//! Now a write is a push onto this queue, which never waits on the network.
//! One thread per connection drains it into the socket under a write timeout.
//! A client that falls further behind than [`LIMIT`] bytes, or whose socket
//! does not take a write within [`WRITE_TIMEOUT`], is disconnected: the
//! socket is shut down, which also wakes its own thread's read, and the
//! reason is kept for the log.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::proto::Conn;

/// Queued bytes past which a client is disconnected rather than buffered for.
///
/// A join burst is the most a healthy client is ever behind by: a full view
/// radius of uncompressed 1.8 columns is a few tens of megabytes, but those go
/// through [`Outbox::push_paced`], which waits for room instead. Everything
/// else is small and steady, so a backlog this size means the client is not
/// reading.
pub const LIMIT: usize = 32 * 1024 * 1024;

/// How far behind a paced sender lets the queue get before it waits.
const PACE: usize = 2 * 1024 * 1024;

/// How long one write may block before the client is given up on.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

struct Queue {
    frames: VecDeque<Vec<u8>>,
    bytes: usize,
    /// Why the connection was closed, once it has been.
    closed: Option<String>,
}

/// One connection's outgoing queue.
pub struct Outbox {
    queue: Mutex<Queue>,
    /// Signalled when frames arrive or the queue closes (for the writer).
    ready: Condvar,
    /// Signalled when the writer frees room or the queue closes (for paced
    /// senders).
    room: Condvar,
    /// Framing of the socket the writer drains into.
    threshold: Option<usize>,
    /// A handle on the socket, kept to shut it down.
    socket: Conn,
}

impl Outbox {
    /// Start draining into `conn` on a thread of its own. `name` is for the
    /// thread's name only.
    pub fn start(conn: Conn, name: &str) -> io::Result<Arc<Outbox>> {
        conn.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let outbox = Arc::new(Outbox {
            queue: Mutex::new(Queue {
                frames: VecDeque::new(),
                bytes: 0,
                closed: None,
            }),
            ready: Condvar::new(),
            room: Condvar::new(),
            threshold: conn.threshold(),
            socket: conn.try_clone()?,
        });
        let writer = Arc::clone(&outbox);
        let mut conn = conn;
        std::thread::Builder::new()
            .name(format!("send-{name}"))
            .spawn(move || writer.drain(&mut conn))?;
        Ok(outbox)
    }

    /// The framing threshold of this connection.
    pub fn threshold(&self) -> Option<usize> {
        self.threshold
    }

    /// Queue `frames` to go out together, never waiting. Returns `false` if
    /// the connection is closed — by this push, if it took the queue past
    /// [`LIMIT`].
    pub fn push(&self, frames: Vec<Vec<u8>>) -> bool {
        let mut q = self.queue.lock().unwrap();
        if q.closed.is_some() {
            return false;
        }
        let size: usize = frames.iter().map(Vec::len).sum();
        if q.bytes + size > LIMIT {
            let reason = format!("not reading ({} MiB queued)", q.bytes >> 20);
            drop(q);
            self.close(&reason);
            return false;
        }
        q.bytes += size;
        q.frames.extend(frames);
        self.ready.notify_one();
        true
    }

    /// Queue `frames`, first waiting while more than [`PACE`] bytes are
    /// queued. For a sender with a lot to say to this one client — its chunk
    /// stream — which should go at the client's pace rather than fill the
    /// queue. Gives up, closing the connection, after [`WRITE_TIMEOUT`].
    pub fn push_paced(&self, frames: Vec<Vec<u8>>) -> bool {
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let mut q = self.queue.lock().unwrap();
        while q.closed.is_none() && q.bytes > PACE {
            let now = Instant::now();
            if now >= deadline {
                drop(q);
                self.close("stopped reading its chunks");
                return false;
            }
            q = self.room.wait_timeout(q, deadline - now).unwrap().0;
        }
        drop(q);
        self.push(frames)
    }

    /// Close the connection: drop what is queued, shut the socket down (which
    /// wakes its reader), and remember why. The first reason given is kept.
    pub fn close(&self, reason: &str) {
        let mut q = self.queue.lock().unwrap();
        if q.closed.is_none() {
            q.closed = Some(reason.to_string());
        }
        q.frames.clear();
        q.bytes = 0;
        drop(q);
        self.socket.shutdown();
        self.ready.notify_all();
        self.room.notify_all();
    }

    /// Why the connection was closed, if it has been.
    pub fn closed(&self) -> Option<String> {
        self.queue.lock().unwrap().closed.clone()
    }

    /// Bytes waiting to be written.
    pub fn queued(&self) -> usize {
        self.queue.lock().unwrap().bytes
    }

    /// The writer thread: take whatever is queued and write it, until closed.
    fn drain(&self, conn: &mut Conn) {
        loop {
            let batch: Vec<Vec<u8>> = {
                let mut q = self.queue.lock().unwrap();
                while q.frames.is_empty() && q.closed.is_none() {
                    q = self.ready.wait(q).unwrap();
                }
                if q.closed.is_some() {
                    return;
                }
                q.frames.drain(..).collect()
            };
            let mut written = 0;
            for frame in &batch {
                if let Err(e) = conn.write_frame(frame) {
                    let reason = match e.kind() {
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                            format!(
                                "not reading (a write blocked for {}s)",
                                WRITE_TIMEOUT.as_secs()
                            )
                        }
                        _ => format!("write failed: {e}"),
                    };
                    self.close(&reason);
                    return;
                }
                written += frame.len();
            }
            let mut q = self.queue.lock().unwrap();
            q.bytes = q.bytes.saturating_sub(written);
            drop(q);
            self.room.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};

    fn pair() -> (Conn, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (server, _) = l.accept().unwrap();
        (Conn::new(server), client)
    }

    #[test]
    fn frames_arrive_in_order() {
        let (conn, mut client) = pair();
        let out = Outbox::start(conn, "t").unwrap();
        assert!(out.push(vec![b"ab".to_vec(), b"c".to_vec()]));
        assert!(out.push(vec![b"de".to_vec()]));
        let mut got = [0u8; 5];
        client.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"abcde");
    }

    #[test]
    fn a_client_that_never_reads_is_cut_off_without_blocking_anyone() {
        // The bug: a peer that stops reading used to block every thread that
        // wrote to it. Push far more than any socket buffer holds and require
        // every push to return at once, and the connection to end closed.
        let (conn, client) = pair();
        let out = Outbox::start(conn, "t").unwrap();
        let chunk = vec![0u8; 1024 * 1024];
        let start = Instant::now();
        let mut accepted = 0;
        for _ in 0..(LIMIT >> 20) + 64 {
            if out.push(vec![chunk.clone()]) {
                accepted += 1;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "pushes never wait"
        );
        assert!(accepted < (LIMIT >> 20) + 64, "the backlog was refused");
        let reason = out.closed().expect("the connection is closed");
        assert!(reason.contains("not reading"), "{reason}");
        assert!(!out.push(vec![b"x".to_vec()]), "nothing more is queued");
        drop(client);
    }

    #[test]
    fn closing_wakes_a_paced_sender() {
        let (conn, _client) = pair();
        let out = Outbox::start(conn, "t").unwrap();
        // Fill past the pace without the client reading.
        let big = vec![0u8; PACE + 1];
        let _ = out.push(vec![big.clone(), big]);
        let waiter = {
            let out = Arc::clone(&out);
            std::thread::spawn(move || out.push_paced(vec![b"x".to_vec()]))
        };
        std::thread::sleep(Duration::from_millis(200));
        out.close("test");
        assert!(!waiter.join().unwrap());
        assert_eq!(out.closed().as_deref(), Some("test"));
    }
}
