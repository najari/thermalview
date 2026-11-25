//! TCP listener and per-client threads. Commands are parsed here and handed to the UI thread,
//! which owns all application state and answers through a one-shot channel.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use super::{parse, reply, Request, MAX_LINE};

/// Clients served at once; more are refused with an error line.
pub const MAX_CLIENTS: usize = 4;
/// How long a client waits for the UI thread to answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);
/// Poll interval for the stop flag in the accept loop and in idle client reads.
const POLL: Duration = Duration::from_millis(100);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

pub type Reply = Result<String, String>;

/// A parsed command waiting for the UI thread.
pub struct Pending {
    pub request: Request,
    pub peer: SocketAddr,
    tx: SyncSender<Reply>,
}

impl Pending {
    pub fn respond(self, r: Reply) {
        // The client may have given up (timeout) or disconnected; nothing to do then.
        let _ = self.tx.send(r);
    }
}

type Wake = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    stop: AtomicBool,
    clients: AtomicUsize,
    wake: Wake,
}

/// Running server. Dropping it stops listening and disconnects every client.
pub struct Server {
    addr: SocketAddr,
    shared: Arc<Shared>,
    rx: Receiver<Pending>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Listen on `port` (0 = any free port), on loopback only unless `allow_lan`.
    /// `wake` is called whenever a command is waiting, so the UI thread runs.
    pub fn start(
        port: u16,
        allow_lan: bool,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let ip = if allow_lan {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        let listener = TcpListener::bind((ip, port))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            clients: AtomicUsize::new(0),
            wake: Arc::new(wake),
        });
        let (tx, rx) = mpsc::channel();
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("remote".into())
            .spawn(move || accept_loop(listener, s, tx))?;
        log::info!("remote control listening on {addr}");
        Ok(Self {
            addr,
            shared,
            rx,
            thread: Some(thread),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn clients(&self) -> usize {
        self.shared.clients.load(Ordering::Relaxed)
    }

    /// Next command waiting for an answer, if any.
    pub fn try_recv(&self) -> Option<Pending> {
        self.rx.try_recv().ok()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        log::info!("remote control stopped ({})", self.addr);
    }
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>, tx: Sender<Pending>) {
    while !shared.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                if shared.clients.load(Ordering::SeqCst) >= MAX_CLIENTS {
                    log::warn!("remote client {peer} refused: too many clients");
                    let _ = refuse(stream);
                    continue;
                }
                shared.clients.fetch_add(1, Ordering::SeqCst);
                let (s, tx) = (shared.clone(), tx.clone());
                let spawned = std::thread::Builder::new()
                    .name(format!("remote {peer}"))
                    .spawn(move || {
                        log::info!("remote client connected: {peer}");
                        if let Err(e) = serve(stream, peer, &s, &tx) {
                            log::info!("remote client {peer}: {e}");
                        }
                        log::info!("remote client disconnected: {peer}");
                        s.clients.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    shared.clients.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(e) => {
                log::warn!("remote accept failed: {e}");
                std::thread::sleep(POLL);
            }
        }
    }
}

fn refuse(mut stream: TcpStream) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    stream.write_all(b"ERR too many clients\r\n")
}

fn serve(
    mut stream: TcpStream,
    peer: SocketAddr,
    shared: &Shared,
    tx: &Sender<Pending>,
) -> io::Result<()> {
    // Accepted sockets inherit non-blocking mode from the listener on Windows.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(POLL))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    stream.set_nodelay(true)?;
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let n = match stream.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        pending.extend_from_slice(&chunk[..n]);
        while let Some(end) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line);
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let (answer, quit) = answer(text, peer, shared, tx);
            stream.write_all(format!("{answer}\r\n").as_bytes())?;
            if quit {
                return Ok(());
            }
        }
        if pending.len() > MAX_LINE {
            stream.write_all(b"ERR line too long\r\n")?;
            return Ok(());
        }
    }
}

/// Reply line for one command, and whether the connection should close.
fn answer(text: &str, peer: SocketAddr, shared: &Shared, tx: &Sender<Pending>) -> (String, bool) {
    if text.len() > MAX_LINE {
        return (reply(&Err("line too long".into())), false);
    }
    let request = match parse(text) {
        Ok(Request::Quit) => return ("OK bye".into(), true),
        Ok(r) => r,
        Err(e) => return (reply(&Err(e)), false),
    };
    let (rtx, rrx) = mpsc::sync_channel(1);
    let job = Pending {
        request,
        peer,
        tx: rtx,
    };
    if tx.send(job).is_err() {
        return (reply(&Err("server stopping".into())), true);
    }
    (shared.wake)();
    match rrx.recv_timeout(REPLY_TIMEOUT) {
        Ok(r) => (reply(&r), false),
        Err(_) => (reply(&Err("application not responding".into())), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    /// Answer every waiting command from a helper thread, like the UI thread would.
    fn echo(server: Server) -> (SocketAddr, Arc<AtomicBool>, JoinHandle<()>) {
        let addr = server.addr();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let t = std::thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                while let Some(p) = server.try_recv() {
                    let r = format!("{:?}", p.request);
                    p.respond(Ok(r));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        (addr, stop, t)
    }

    fn line(r: &mut BufReader<TcpStream>) -> String {
        let mut s = String::new();
        r.read_line(&mut s).unwrap();
        s.trim_end().to_string()
    }

    #[test]
    fn commands_round_trip_over_tcp() {
        let server = Server::start(0, false, || {}).unwrap();
        assert!(server.addr().ip().is_loopback());
        let (addr, stop, t) = echo(server);

        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut r = BufReader::new(c.try_clone().unwrap());
        // Two commands in one packet, one split across packets, CRLF and LF endings.
        c.write_all(b"meas\r\nTEMP 1 2\nspot ").unwrap();
        assert_eq!(line(&mut r), "OK Measure");
        assert_eq!(line(&mut r), "OK Temp { x: 1, y: 2 }");
        c.write_all(b"add 3 4\n\nbogus\n").unwrap();
        assert_eq!(line(&mut r), "OK SpotAdd { x: 3, y: 4 }");
        assert!(line(&mut r).starts_with("ERR unknown command BOGUS"));
        c.write_all(b"QUIT\n").unwrap();
        assert_eq!(line(&mut r), "OK bye");
        assert_eq!(line(&mut r), "", "server closes after QUIT");

        stop.store(true, Ordering::SeqCst);
        t.join().unwrap();
    }

    #[test]
    fn long_line_is_rejected_and_closed() {
        let server = Server::start(0, false, || {}).unwrap();
        let (addr, stop, t) = echo(server);
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(&vec![b'A'; MAX_LINE + 10]).unwrap();
        let mut r = BufReader::new(c);
        assert_eq!(line(&mut r), "ERR line too long");
        assert_eq!(line(&mut r), "");
        stop.store(true, Ordering::SeqCst);
        t.join().unwrap();
    }

    #[test]
    fn unanswered_command_times_out_and_drop_disconnects() {
        let server = Server::start(0, false, || {}).unwrap();
        let mut c = TcpStream::connect(server.addr()).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.write_all(b"STATUS\n").unwrap();
        let mut r = BufReader::new(c);
        assert_eq!(line(&mut r), "ERR application not responding");
        drop(server);
        assert_eq!(
            line(&mut r),
            "",
            "client is disconnected when the server stops"
        );
    }

    #[test]
    fn extra_clients_are_refused() {
        let server = Server::start(0, false, || {}).unwrap();
        let (addr, stop, t) = echo(server);
        let mut held = Vec::new();
        for _ in 0..MAX_CLIENTS {
            let mut c = TcpStream::connect(addr).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            // A round trip proves the client thread is running and counted.
            c.write_all(b"STATUS\n").unwrap();
            let mut r = BufReader::new(c.try_clone().unwrap());
            assert_eq!(line(&mut r), "OK Status");
            held.push(c);
        }
        let c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut r = BufReader::new(c);
        assert_eq!(line(&mut r), "ERR too many clients");
        stop.store(true, Ordering::SeqCst);
        t.join().unwrap();
    }
}
