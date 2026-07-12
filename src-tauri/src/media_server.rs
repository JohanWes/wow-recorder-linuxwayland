use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

pub struct MediaServer {
    port: u16,
    files: Arc<Mutex<HashMap<String, PathBuf>>>,
    stop: Arc<AtomicBool>,
}

impl MediaServer {
    pub fn start() -> io::Result<Self> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let files = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_files = Arc::clone(&files);
        let thread_stop = Arc::clone(&stop);
        thread::spawn(move || serve(listener, thread_files, thread_stop));
        Ok(Self { port, files, stop })
    }

    pub fn register(&self, path: PathBuf) -> io::Result<String> {
        // ponytail: listings are small enough that avoiding a second reverse map
        // is preferable to keeping two maps synchronized.
        if let Some((token, _)) = self
            .files
            .lock()
            .unwrap()
            .iter()
            .find(|(_, registered)| *registered == &path)
        {
            return Ok(format!("http://127.0.0.1:{}/video/{token}", self.port));
        }
        let token = random_token()?;
        self.files.lock().unwrap().insert(token.clone(), path);
        Ok(format!("http://127.0.0.1:{}/video/{token}", self.port))
    }

    pub fn clear(&self) {
        self.files.lock().unwrap().clear();
    }
}

impl Drop for MediaServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve(
    listener: TcpListener,
    files: Arc<Mutex<HashMap<String, PathBuf>>>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let files = Arc::clone(&files);
                thread::spawn(move || {
                    let _ = handle_request(stream, &files);
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                eprintln!("media server stopped accepting connections: {error}");
                break;
            }
        }
    }
}

fn handle_request(stream: TcpStream, files: &Mutex<HashMap<String, PathBuf>>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    stream.set_nodelay(true)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    // Large buffer keeps header+body in few big writes; io::copy reuses it
    // directly, so file data reaches the socket in 256K chunks.
    let mut writer = BufWriter::with_capacity(256 * 1024, stream);
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(());
        }
        let mut header_bytes = request_line.len();
        let mut request = request_line.split_whitespace();
        let method = request.next().unwrap_or("").to_owned();
        let token = request
            .next()
            .and_then(|path| path.strip_prefix("/video/"))
            .map(str::to_owned);

        let mut range = None;
        let mut close = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            if line == "\r\n" {
                break;
            }
            header_bytes += line.len();
            if header_bytes > 16 * 1024 {
                return write_empty(&mut writer, "431 Request Header Fields Too Large");
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("range") {
                    range = value.trim().strip_prefix("bytes=").and_then(parse_range);
                } else if name.eq_ignore_ascii_case("connection")
                    && value.trim().eq_ignore_ascii_case("close")
                {
                    close = true;
                }
            }
        }

        if method != "GET" && method != "HEAD" {
            return write_empty(&mut writer, "405 Method Not Allowed");
        }
        let Some(path) = token.and_then(|token| files.lock().unwrap().get(&token).cloned()) else {
            return write_empty(&mut writer, "404 Not Found");
        };
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(_) => return write_empty(&mut writer, "404 Not Found"),
        };
        let file_len = file.metadata()?.len();
        if file_len == 0 {
            write!(
                writer,
                "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nContent-Length: 0\r\nCache-Control: private, max-age=604800, immutable\r\n\r\n"
            )?;
            writer.flush()?;
            if close {
                return Ok(());
            }
            continue;
        }
        let (start, end, partial) = match resolve_range(range, file_len) {
            Some(value) => value,
            None => {
                write!(
                    writer,
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{file_len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )?;
                return writer.flush();
            }
        };
        let content_len = end.saturating_sub(start) + 1;
        let status = if partial {
            "206 Partial Content"
        } else {
            "200 OK"
        };
        write!(
            writer,
            "HTTP/1.1 {status}\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nContent-Length: {content_len}\r\n"
        )?;
        if partial {
            write!(writer, "Content-Range: bytes {start}-{end}/{file_len}\r\n")?;
        }
        write!(
            writer,
            "Cache-Control: private, max-age=604800, immutable\r\n\r\n"
        )?;
        if method == "GET" && content_len > 0 {
            file.seek(SeekFrom::Start(start))?;
            io::copy(&mut file.take(content_len), &mut writer)?;
        }
        writer.flush()?;
        if close {
            return Ok(());
        }
    }
}

fn write_empty(writer: &mut impl Write, status: &str) -> io::Result<()> {
    write!(
        writer,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    writer.flush()
}

fn random_token() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn parse_range(value: &str) -> Option<(Option<u64>, Option<u64>)> {
    if value.contains(',') {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    Some((
        (!start.is_empty()).then(|| start.parse().ok()).flatten(),
        (!end.is_empty()).then(|| end.parse().ok()).flatten(),
    ))
}

fn resolve_range(range: Option<(Option<u64>, Option<u64>)>, len: u64) -> Option<(u64, u64, bool)> {
    let Some((start, end)) = range else {
        return Some((0, len - 1, false));
    };
    match (start, end) {
        (Some(start), end) if start < len && end.is_none_or(|end| end >= start) => {
            Some((start, end.unwrap_or(len - 1).min(len - 1), true))
        }
        (None, Some(suffix)) if suffix > 0 => Some((len.saturating_sub(suffix), len - 1, true)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_range, resolve_range, MediaServer};
    use std::io::{Read, Write};
    use std::net::TcpStream;

    #[test]
    fn parses_and_bounds_http_ranges() {
        assert_eq!(parse_range("10-19"), Some((Some(10), Some(19))));
        assert_eq!(
            resolve_range(parse_range("10-19"), 100),
            Some((10, 19, true))
        );
        assert_eq!(resolve_range(parse_range("90-"), 100), Some((90, 99, true)));
        assert_eq!(resolve_range(parse_range("-10"), 100), Some((90, 99, true)));
        assert_eq!(resolve_range(parse_range("100-"), 100), None);
        assert_eq!(resolve_range(parse_range("20-10"), 100), None);
    }

    #[test]
    fn serves_a_registered_file_with_range_requests() {
        let dir = std::env::temp_dir().join("wr-media-server-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");
        std::fs::write(&path, b"0123456789").unwrap();

        let server = MediaServer::start().unwrap();
        let url = server.register(path).unwrap();
        let (host, token_path) = url
            .strip_prefix("http://")
            .and_then(|rest| rest.split_once('/'))
            .unwrap();

        let request = |range: Option<&str>| {
            let mut stream = TcpStream::connect(host).unwrap();
            let range = range.map_or(String::new(), |r| format!("Range: bytes={r}\r\n"));
            write!(
                stream,
                "GET /{token_path} HTTP/1.1\r\n{range}Connection: close\r\n\r\n"
            )
            .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };

        let full = request(None);
        assert!(full.starts_with("HTTP/1.1 200 OK"), "{full}");
        assert!(full.ends_with("0123456789"), "{full}");

        let partial = request(Some("2-5"));
        assert!(
            partial.starts_with("HTTP/1.1 206 Partial Content"),
            "{partial}"
        );
        assert!(partial.contains("Content-Range: bytes 2-5/10"), "{partial}");
        assert!(partial.ends_with("2345"), "{partial}");

        let bad = request(Some("99-"));
        assert!(bad.starts_with("HTTP/1.1 416"), "{bad}");
    }

    #[test]
    fn serves_two_requests_on_one_connection() {
        let dir = std::env::temp_dir().join("wr-media-server-keepalive-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");
        std::fs::write(&path, b"0123456789").unwrap();

        let server = MediaServer::start().unwrap();
        let url = server.register(path).unwrap();
        let (host, token_path) = url
            .strip_prefix("http://")
            .unwrap()
            .split_once('/')
            .unwrap();
        let mut stream = TcpStream::connect(host).unwrap();
        write!(stream, "GET /{token_path} HTTP/1.1\r\nRange: bytes=0-1\r\n\r\nGET /{token_path} HTTP/1.1\r\nRange: bytes=8-9\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();

        assert_eq!(
            response.matches("HTTP/1.1 206 Partial Content").count(),
            2,
            "{response}"
        );
        assert!(response.contains("\r\n\r\n01HTTP/1.1 206"), "{response}");
        assert!(response.ends_with("89"), "{response}");
    }
}
