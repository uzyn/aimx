use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_rustls::TlsAcceptor;

use crate::config::Config;

const MAX_COMMAND_LINE_LENGTH: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Connected,
    Greeted,
    MailFrom,
    RcptTo,
    Data,
}

enum CommandLoopExit {
    Done,
    StarttlsUpgrade,
    Err(Box<dyn std::error::Error + Send + Sync>),
}

pub struct SessionParams {
    pub config: Arc<Config>,
    pub tls_acceptor: Option<Arc<TlsAcceptor>>,
    pub hostname: String,
    pub peer_addr: SocketAddr,
    pub max_message_size: usize,
    pub idle_timeout: Duration,
    pub total_timeout: Duration,
    pub max_commands_before_data: usize,
}

pub struct SmtpSession {
    params: SessionParams,
}

struct SessionState {
    state: State,
    reverse_path: String,
    forward_paths: Vec<String>,
    tls_active: bool,
    ehlo_hostname: String,
    command_count: usize,
    total_bytes: usize,
    message_count: usize,
}

impl SessionState {
    fn new() -> Self {
        Self {
            state: State::Connected,
            reverse_path: String::new(),
            forward_paths: Vec::new(),
            tls_active: false,
            ehlo_hostname: String::new(),
            command_count: 0,
            total_bytes: 0,
            message_count: 0,
        }
    }

    fn reset_transaction(&mut self) {
        self.reverse_path.clear();
        self.forward_paths.clear();
        if self.state != State::Connected {
            self.state = State::Greeted;
        }
    }
}

impl SmtpSession {
    pub fn new(params: SessionParams) -> Self {
        Self { params }
    }

    pub async fn handle(
        self,
        stream: tokio::net::TcpStream,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let connection_start = Instant::now();
        let total_deadline = tokio::time::Instant::now() + self.params.total_timeout;

        let mut session_state = SessionState::new();
        let banner = format!("220 {} ESMTP aimx\r\n", self.params.hostname);

        let result = tokio::time::timeout_at(total_deadline, async {
            self.run_session(stream, &mut session_state, &banner, total_deadline)
                .await
        })
        .await;

        let duration = connection_start.elapsed();
        match &result {
            Ok(Ok(_)) => {
                eprintln!(
                    "[{}] Connection closed ehlo={} messages={} bytes={} duration={:.1}s result=ok",
                    self.params.peer_addr,
                    session_state.ehlo_hostname,
                    session_state.message_count,
                    session_state.total_bytes,
                    duration.as_secs_f64()
                );
            }
            Ok(Err(e)) => {
                eprintln!(
                    "[{}] Connection error ehlo={} messages={} bytes={} duration={:.1}s result=error: {}",
                    self.params.peer_addr,
                    session_state.ehlo_hostname,
                    session_state.message_count,
                    session_state.total_bytes,
                    duration.as_secs_f64(),
                    e
                );
            }
            Err(_) => {
                eprintln!(
                    "[{}] Connection timeout ehlo={} messages={} bytes={} duration={:.1}s result=timeout",
                    self.params.peer_addr,
                    session_state.ehlo_hostname,
                    session_state.message_count,
                    session_state.total_bytes,
                    duration.as_secs_f64()
                );
            }
        }

        result.unwrap_or(Ok(()))
    }

    async fn run_session(
        &self,
        stream: tokio::net::TcpStream,
        session_state: &mut SessionState,
        banner: &str,
        total_deadline: tokio::time::Instant,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (reader, mut writer) = tokio::io::split(stream);
        let mut reader = BufReader::new(reader);
        writer.write_all(banner.as_bytes()).await?;

        match self
            .command_loop(&mut reader, &mut writer, session_state, total_deadline)
            .await
        {
            CommandLoopExit::StarttlsUpgrade => {
                let inner = reader.into_inner().unsplit(writer);
                self.handle_tls_upgrade(inner, session_state, total_deadline)
                    .await
            }
            CommandLoopExit::Done => Ok(()),
            CommandLoopExit::Err(e) => Err(e),
        }
    }

    async fn handle_tls_upgrade(
        &self,
        stream: tokio::net::TcpStream,
        session_state: &mut SessionState,
        total_deadline: tokio::time::Instant,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let acceptor = self
            .params
            .tls_acceptor
            .as_ref()
            .expect("TLS acceptor must exist during STARTTLS upgrade");
        let tls_stream = match acceptor.accept(stream).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[{}] TLS handshake failed: {}", self.params.peer_addr, e);
                return Err(format!("TLS handshake failed: {e}").into());
            }
        };
        session_state.tls_active = true;
        session_state.state = State::Connected;
        session_state.reset_transaction();

        let (reader, mut writer) = tokio::io::split(tls_stream);
        let mut reader = BufReader::new(reader);

        match self
            .command_loop(&mut reader, &mut writer, session_state, total_deadline)
            .await
        {
            CommandLoopExit::Done | CommandLoopExit::StarttlsUpgrade => Ok(()),
            CommandLoopExit::Err(e) => Err(e),
        }
    }

    async fn command_loop<R, W>(
        &self,
        reader: &mut BufReader<R>,
        writer: &mut W,
        session_state: &mut SessionState,
        total_deadline: tokio::time::Instant,
    ) -> CommandLoopExit
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut line_buf = String::new();
        loop {
            line_buf.clear();
            let read_result = tokio::time::timeout(self.params.idle_timeout, async {
                tokio::time::timeout_at(
                    total_deadline,
                    bounded_read_line(reader, &mut line_buf, MAX_COMMAND_LINE_LENGTH),
                )
                .await
            })
            .await;

            let line_result = match read_result {
                Ok(Ok(r)) => r,
                Ok(Err(_)) => {
                    let _ = writer.write_all(b"421 Connection timed out\r\n").await;
                    return CommandLoopExit::Done;
                }
                Err(_) => {
                    let _ = writer.write_all(b"421 Idle timeout exceeded\r\n").await;
                    return CommandLoopExit::Done;
                }
            };

            match line_result {
                ReadLineResult::TooLong => {
                    if writer.write_all(b"500 Line too long\r\n").await.is_err() {
                        return CommandLoopExit::Done;
                    }
                    continue;
                }
                ReadLineResult::Eof => return CommandLoopExit::Done,
                ReadLineResult::Err(e) => {
                    return CommandLoopExit::Err(format!("Read error: {e}").into());
                }
                ReadLineResult::Ok(_) => {}
            }

            let line = line_buf.trim_end();
            if line.is_empty() {
                continue;
            }

            session_state.command_count += 1;
            if session_state.state != State::Data
                && session_state.command_count > self.params.max_commands_before_data
            {
                let _ = writer.write_all(b"421 Too many commands\r\n").await;
                return CommandLoopExit::Done;
            }

            let (cmd, args) = parse_command(line);
            let response = match cmd.as_str() {
                "EHLO" => self.handle_ehlo(args, session_state),
                "HELO" => self.handle_helo(args, session_state),
                "MAIL" => self.handle_mail_from(args, session_state),
                "RCPT" => self.handle_rcpt_to(args, session_state),
                "DATA" => {
                    if let Some(resp) = self.handle_data_precheck(session_state) {
                        resp
                    } else {
                        if writer
                            .write_all(b"354 Start mail input; end with <CRLF>.<CRLF>\r\n")
                            .await
                            .is_err()
                        {
                            return CommandLoopExit::Done;
                        }
                        session_state.state = State::Data;
                        let result = match self
                            .receive_data(reader, session_state, total_deadline)
                            .await
                        {
                            Ok(r) => r,
                            Err(e) => return CommandLoopExit::Err(e),
                        };
                        session_state.reset_transaction();
                        result
                    }
                }
                "RSET" => self.handle_rset(session_state),
                "NOOP" => "250 OK\r\n".to_string(),
                "QUIT" => {
                    let _ = writer.write_all(b"221 Bye\r\n").await;
                    return CommandLoopExit::Done;
                }
                "STARTTLS" => {
                    if self.params.tls_acceptor.is_none() {
                        "502 STARTTLS not available\r\n".to_string()
                    } else if session_state.tls_active {
                        "503 TLS already active\r\n".to_string()
                    } else {
                        let _ = writer.write_all(b"220 Ready to start TLS\r\n").await;
                        return CommandLoopExit::StarttlsUpgrade;
                    }
                }
                _ => "500 Unrecognized command\r\n".to_string(),
            };

            if writer.write_all(response.as_bytes()).await.is_err() {
                return CommandLoopExit::Done;
            }
        }
    }

    fn handle_ehlo(&self, args: &str, session_state: &mut SessionState) -> String {
        if args.is_empty() {
            return "501 EHLO requires domain argument\r\n".to_string();
        }
        session_state.ehlo_hostname = args.to_string();
        session_state.state = State::Greeted;
        session_state.reset_transaction();

        let mut response = format!("250-{} Hello {}\r\n", self.params.hostname, args);
        response.push_str(&format!("250-SIZE {}\r\n", self.params.max_message_size));
        response.push_str("250-8BITMIME\r\n");
        response.push_str("250-PIPELINING\r\n");
        if self.params.tls_acceptor.is_some() && !session_state.tls_active {
            response.push_str("250-STARTTLS\r\n");
        }
        response.push_str("250 SMTPUTF8\r\n");
        response
    }

    fn handle_helo(&self, args: &str, session_state: &mut SessionState) -> String {
        if args.is_empty() {
            return "501 HELO requires domain argument\r\n".to_string();
        }
        session_state.ehlo_hostname = args.to_string();
        session_state.state = State::Greeted;
        session_state.reset_transaction();
        format!("250 {} Hello {}\r\n", self.params.hostname, args)
    }

    fn handle_mail_from(&self, args: &str, session_state: &mut SessionState) -> String {
        if session_state.state == State::Connected {
            return "503 Send EHLO/HELO first\r\n".to_string();
        }
        if session_state.state == State::MailFrom || session_state.state == State::RcptTo {
            return "503 MAIL FROM already given\r\n".to_string();
        }
        let upper = args.to_uppercase();
        if !upper.starts_with("FROM:") {
            return "501 Syntax: MAIL FROM:<address>\r\n".to_string();
        }
        let addr = extract_angle_addr(&args[5..]);
        session_state.reverse_path = addr;
        session_state.state = State::MailFrom;
        "250 OK\r\n".to_string()
    }

    fn handle_rcpt_to(&self, args: &str, session_state: &mut SessionState) -> String {
        if session_state.state == State::Connected || session_state.state == State::Greeted {
            return "503 Send MAIL FROM first\r\n".to_string();
        }
        let upper = args.to_uppercase();
        if !upper.starts_with("TO:") {
            return "501 Syntax: RCPT TO:<address>\r\n".to_string();
        }
        let addr = extract_angle_addr(&args[3..]);
        if addr.is_empty() {
            return "501 Syntax: RCPT TO:<address>\r\n".to_string();
        }
        session_state.forward_paths.push(addr);
        session_state.state = State::RcptTo;
        "250 OK\r\n".to_string()
    }

    fn handle_data_precheck(&self, session_state: &SessionState) -> Option<String> {
        if session_state.state != State::RcptTo {
            return Some("503 Send RCPT TO first\r\n".to_string());
        }
        None
    }

    fn handle_rset(&self, session_state: &mut SessionState) -> String {
        session_state.reset_transaction();
        "250 OK\r\n".to_string()
    }

    async fn receive_data<R>(
        &self,
        reader: &mut BufReader<R>,
        session_state: &mut SessionState,
        total_deadline: tokio::time::Instant,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>
    where
        R: AsyncRead + Unpin,
    {
        let mut data = Vec::new();
        let mut line_buf = String::new();
        let data_line_limit = self.params.max_message_size + 1024;

        loop {
            line_buf.clear();
            let read_result = tokio::time::timeout(self.params.idle_timeout, async {
                tokio::time::timeout_at(
                    total_deadline,
                    bounded_read_line(reader, &mut line_buf, data_line_limit),
                )
                .await
            })
            .await;

            let line_result = match read_result {
                Ok(Ok(r)) => r,
                Ok(Err(_)) | Err(_) => {
                    return Ok("421 Timeout during DATA\r\n".to_string());
                }
            };

            match line_result {
                ReadLineResult::TooLong => {
                    // Drain remaining DATA lines
                    loop {
                        line_buf.clear();
                        match tokio::time::timeout(
                            self.params.idle_timeout,
                            bounded_read_line(reader, &mut line_buf, data_line_limit),
                        )
                        .await
                        {
                            Ok(ReadLineResult::Ok(_)) if line_buf.trim_end() == "." => break,
                            Ok(ReadLineResult::Eof) | Err(_) => break,
                            _ => continue,
                        }
                    }
                    return Ok("552 Message exceeds maximum size\r\n".to_string());
                }
                ReadLineResult::Eof => {
                    return Ok("451 Client disconnected during DATA\r\n".to_string());
                }
                ReadLineResult::Err(e) => {
                    return Err(format!("Read error during DATA: {e}").into());
                }
                ReadLineResult::Ok(_) => {}
            }

            // RFC 5321: reject bare LF (lines must end with CRLF)
            if line_buf.ends_with('\n') && !line_buf.ends_with("\r\n") {
                return Ok("500 Bare LF not allowed (RFC 5321)\r\n".to_string());
            }

            if line_buf.trim_end() == "." {
                break;
            }

            // Dot-stuffing: remove leading dot per RFC 5321 section 4.5.2
            if line_buf.starts_with("..") {
                data.extend_from_slice(&line_buf.as_bytes()[1..]);
            } else {
                data.extend_from_slice(line_buf.as_bytes());
            }

            if data.len() > self.params.max_message_size {
                loop {
                    line_buf.clear();
                    match tokio::time::timeout(
                        self.params.idle_timeout,
                        bounded_read_line(reader, &mut line_buf, data_line_limit),
                    )
                    .await
                    {
                        Ok(ReadLineResult::Ok(_)) if line_buf.trim_end() == "." => break,
                        Ok(ReadLineResult::Eof) | Err(_) => break,
                        _ => continue,
                    }
                }
                return Ok("552 Message exceeds maximum size\r\n".to_string());
            }
        }

        self.deliver_message(&data, session_state).await
    }

    async fn deliver_message(
        &self,
        data: &[u8],
        session_state: &mut SessionState,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let config = Arc::clone(&self.params.config);
        // One allocation shared across all recipients via refcount. For large
        // DATA payloads (up to message_max) with many RCPT TOs, this avoids
        // an N-way copy of the full message.
        let data = Arc::new(data.to_vec());
        let mut succeeded = 0usize;
        let mut failed = 0usize;

        for rcpt in &session_state.forward_paths {
            let config = Arc::clone(&config);
            let data = Arc::clone(&data);
            let rcpt_owned = rcpt.clone();
            let peer = self.params.peer_addr;
            let reverse_path = session_state.reverse_path.clone();

            let result = tokio::task::spawn_blocking(move || {
                let envelope = if reverse_path.is_empty() {
                    None
                } else {
                    Some(reverse_path.as_str())
                };
                crate::ingest::ingest_email(&config, &rcpt_owned, &data, peer.ip(), envelope)
                    .map_err(|e| e.to_string())
            })
            .await;

            match result {
                Ok(Ok(())) => succeeded += 1,
                Ok(Err(e)) => {
                    eprintln!("[{peer}] Ingest failed for {rcpt}: {e}");
                    failed += 1;
                }
                Err(e) => {
                    eprintln!("[{peer}] Ingest task panicked for {rcpt}: {e}");
                    failed += 1;
                }
            }
        }

        let size = data.len();
        let rcpt_count = session_state.forward_paths.len();
        if succeeded > 0 {
            session_state.message_count += 1;
            session_state.total_bytes += size;
            if failed > 0 {
                eprintln!(
                    "[{}] Message partially accepted from={} rcpts={} succeeded={} failed={} size={}",
                    self.params.peer_addr,
                    session_state.reverse_path,
                    rcpt_count,
                    succeeded,
                    failed,
                    size
                );
            } else {
                eprintln!(
                    "[{}] Message accepted from={} rcpts={} size={}",
                    self.params.peer_addr, session_state.reverse_path, rcpt_count, size
                );
            }
            Ok("250 OK message accepted\r\n".to_string())
        } else {
            eprintln!(
                "[{}] Message rejected from={} rcpts={} all_failed size={}",
                self.params.peer_addr, session_state.reverse_path, rcpt_count, size
            );
            Ok("451 Temporary failure, please retry\r\n".to_string())
        }
    }
}

enum ReadLineResult {
    Ok(()),
    TooLong,
    Eof,
    Err(String),
}

async fn bounded_read_line<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    buf: &mut String,
    max_len: usize,
) -> ReadLineResult {
    loop {
        let available = match reader.fill_buf().await {
            Ok([]) => return ReadLineResult::Eof,
            Ok(b) => b,
            Err(e) => return ReadLineResult::Err(e.to_string()),
        };
        if let Some(newline_pos) = available.iter().position(|&b| b == b'\n') {
            let line_len = buf.len() + newline_pos + 1;
            if line_len > max_len {
                reader.consume(newline_pos + 1);
                return ReadLineResult::TooLong;
            }
            let chunk = &available[..newline_pos + 1];
            buf.push_str(&String::from_utf8_lossy(chunk));
            reader.consume(newline_pos + 1);
            return ReadLineResult::Ok(());
        }
        // No newline in the buffer yet
        if buf.len() + available.len() > max_len {
            let n = available.len();
            reader.consume(n);
            // Drain until newline or EOF
            loop {
                let rest = match reader.fill_buf().await {
                    Ok([]) => return ReadLineResult::TooLong,
                    Ok(b) => b,
                    Err(_) => return ReadLineResult::TooLong,
                };
                if let Some(pos) = rest.iter().position(|&b| b == b'\n') {
                    reader.consume(pos + 1);
                    return ReadLineResult::TooLong;
                }
                let rn = rest.len();
                reader.consume(rn);
            }
        }
        let chunk = String::from_utf8_lossy(available).to_string();
        let n = available.len();
        buf.push_str(&chunk);
        reader.consume(n);
    }
}

fn parse_command(line: &str) -> (String, &str) {
    let line = line.trim();
    if let Some(pos) = line.find(' ') {
        let cmd = line[..pos].to_uppercase();
        let args = line[pos + 1..].trim();
        (cmd, args)
    } else {
        (line.to_uppercase(), "")
    }
}

fn extract_angle_addr(s: &str) -> String {
    let s = s.trim();
    if let Some(start) = s.find('<')
        && let Some(end) = s.find('>')
    {
        return s[start + 1..end].trim().to_string();
    }
    s.to_string()
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn test_parse_command_with_args() {
        let (cmd, args) = parse_command("EHLO example.com");
        assert_eq!(cmd, "EHLO");
        assert_eq!(args, "example.com");
    }

    #[test]
    fn test_parse_command_no_args() {
        let (cmd, args) = parse_command("QUIT");
        assert_eq!(cmd, "QUIT");
        assert_eq!(args, "");
    }

    #[test]
    fn test_parse_command_case_insensitive() {
        let (cmd, _) = parse_command("ehlo test.com");
        assert_eq!(cmd, "EHLO");
    }

    #[test]
    fn test_extract_angle_addr() {
        assert_eq!(extract_angle_addr("<user@example.com>"), "user@example.com");
        assert_eq!(
            extract_angle_addr(" <user@example.com> "),
            "user@example.com"
        );
        assert_eq!(extract_angle_addr("<>"), "");
        assert_eq!(extract_angle_addr("user@example.com"), "user@example.com");
    }
}
