# aimx codebase audit: cross-platform (macOS/Windows) and relay (no port 25) readiness

All repo citations are `path:line` at commit `e00966c74f6cee4b7821f2d7b8a5717961eee76c`. Each resolves to `https://github.com/uzyn/aimx/blob/e00966c74f6cee4b7821f2d7b8a5717961eee76c/<path>#L<line>`. Line numbers come from production code (outside `mod tests`) unless marked "(test)". Third-party crate facts come from crate sources downloaded from `https://static.crates.io/crates/<name>/<name>-<ver>.crate` at the versions pinned in `Cargo.lock`. docs.rs and learn.microsoft.com were unreachable from the sandbox (proxy 403 / DNS failure).

## Q1. Which Unix-only APIs, crates, and OS assumptions are used, grouped by module?

### Takeaway
Unix assumptions sit in about 25 modules. They concentrate in five places: `platform.rs` (hook sandbox: systemd-run or fork+setuid), `serve.rs` (port 25, `/run/aimx` UDS at mode 0o666, SIGTERM/SIGHUP, systemd/OpenRC units, journalctl), `uds_authz.rs` + `auth.rs` (SO_PEERCRED uid authz), `ownership.rs` + `user_resolver.rs` (chown/chmod/getpwnam: every mailbox is owned by a real Unix user), and `setup.rs` (useradd, openssl, ss, dig, curl, `hostname -I`, systemctl/rc-service). There is partial portability scaffolding (`#[cfg(not(unix))]` stubs in 10 files), but several unconditional `std::os::unix` / `tokio::net::UnixStream` / `libc::getpw*` uses mean the crate almost certainly does not compile for Windows today. macOS is much closer: libc, UDS, and peer_cred all exist there. The blockers on macOS are init system, user creation, Linux-only CLI tools, and `/run` paths.

### Cited Findings
**Existing cfg gating (scaffolding)**
- `#[cfg(unix)]` / `#[cfg(not(unix))]` gates exist in: serve.rs (15), setup.rs (10), platform.rs (8), dkim.rs (7), agents_setup.rs (5), upgrade.rs (3), send.rs (2), doctor.rs (2), datadir_readme.rs (2), ingest.rs (1). Source: `grep -rn 'cfg((all()?(not()?(unix|windows|target_os|target_family)' src`.
- `platform::is_root()` returns `false` and `platform::current_euid()` returns `0` under `cfg(not(unix))`: `src/platform.rs:22-45` (libc::geteuid at :24 and :38).
- The hook fallback executor is stubbed on non-Unix and returns `Unsupported: "spawn_sandboxed fallback path requires Unix"`. Hooks therefore cannot run at all on Windows today: `src/platform.rs:353-366`.
- DKIM key writes fall back to a plain `std::fs::write` with no mode on non-Unix, so the private key gets no ACL protection: `src/dkim.rs:125-129` (Unix path uses `OpenOptionsExt::mode` at `src/dkim.rs:116-120`; 0o600 private / 0o644 public at `src/dkim.rs:72,77`).
- `set_socket_mode` is a no-op on non-Unix: `src/serve.rs:740-743`.

**platform.rs (hook execution sandbox)**
- `spawn_sandboxed` picks systemd-run when the init system is systemd (`/run/systemd/system` exists). Otherwise it falls back to fork+setuid. `AIMX_SANDBOX_FORCE_FALLBACK` forces the fallback: `src/platform.rs:150-176`. Init detection is at `src/serve.rs:1031`.
- systemd-run invocation: `--uid/--gid=<owner>`, `ProtectSystem=strict`, `PrivateDevices=yes`, `NoNewPrivileges=yes`, `MemoryMax`, `RuntimeMaxSec`, `--setenv` per env var: `src/platform.rs:212-240`.
- Fork fallback: `pre_exec` → `libc::setsid()` (`src/platform.rs:286-287`), then `setgroups(0)`, `setgid`, `setuid` (`src/platform.rs:319-341`). User lookup is via `libc::getpwnam` (`src/platform.rs:515-518`).
- Timeout enforcement sends `kill(-pid, SIGTERM)`, then `SIGKILL` to the process group after `SIGKILL_GRACE` = 5 s: `src/platform.rs:61, 448-462`.
- Hooks always run as `mailbox_config.owner`: `src/hook.rs:479, 495`. Env passed: `AIMX_FROM/SUBJECT/TO/MAILBOX/FILEPATH/MESSAGE_ID/EVENT/ID/DATE/HOOK_NAME`: `src/hook.rs:347-356, 411-416`.

**serve.rs (daemon)**
- `DEFAULT_BIND = "0.0.0.0:25"`, `DEFAULT_TLS_CERT/KEY = /etc/ssl/aimx/{cert,key}.pem`, `DEFAULT_RUNTIME_DIR = "/run/aimx"` (env override `AIMX_RUNTIME_DIR`), socket name `aimx.sock`: `src/serve.rs:248-254`, resolver at `:261-273`.
- Process umask is set to 0o077 via `libc::umask`: `src/serve.rs:449` → `src/ownership.rs:112`.
- Signal loop uses `tokio::signal::unix::signal(SignalKind::terminate())` and `hangup()` with no cfg gate. `ctrl_c()` is portable: `src/serve.rs:629-634`. SIGHUP triggers `reload_config`: `src/serve.rs:647-670`.
- UDS: `bind_send_socket` → `tokio::net::UnixListener::bind`, `set_socket_mode(path, 0o666)` using `PermissionsExt::from_mode`: `src/serve.rs:717-737`. `run_send_listener(listener: tokio::net::UnixListener, …)`: `src/serve.rs:745-750`. `caller_from_stream` (peer_cred) at accept: `src/serve.rs:757-763`. `handle_uds_connection(stream: tokio::net::UnixStream, caller: Caller)`: `src/serve.rs:810-812, 833-835`.
- PID file at `<runtime_dir>/aimx.pid`: `src/serve.rs:608-616, 700-709`.
- `service` submodule: `generate_systemd_unit` (`src/serve.rs:970`), OpenRC script with `checkpath -d -m 0755 -o root:root /run/aimx` (`src/serve.rs:1018`), `detect_init_system` (`src/serve.rs:1026-1035`), systemctl/rc-service restart/stop/start/status command builders (`src/serve.rs:1051-1139`), `journalctl` tail/follow (`src/serve.rs:1158, 1182`), OpenRC log-file tail (`src/serve.rs:1212`), and follow is supported on systemd only (`src/serve.rs:1238`).

**uds_authz.rs / auth.rs (authorization)**
- Unconditional imports of `tokio::net::UnixStream` and `tokio::net::unix::UCred`: `src/uds_authz.rs:20-21`.
- `lookup_username(uid)` via `libc::getpwuid`: `src/uds_authz.rs:150-176`. `caller_from_stream` → `stream.peer_cred()`: `src/uds_authz.rs:182-187`.
- `authorize(caller_uid, action, mailbox)` is pure logic with uid 0 as a blanket bypass: `src/auth.rs:196-243` (details in Q6).

**ownership.rs / user_resolver.rs / config.rs (Unix users as mailbox owners)**
- `chown_as_owner` → `libc::chown` + `libc::chmod`, using `OsStrExt` to build the C path: `src/ownership.rs:29, 73-95`.
- Callers: ingest (`src/ingest.rs:179, 398, 479, 1243`, wrapper at `:1302-1306`), send_handler (`src/send_handler.rs:611, 721`), state_handler (`src/state_handler.rs:279`), mailbox_handler (`src/mailbox_handler.rs:404, 411`), mailbox CLI (`src/mailbox.rs:231-240`).
- `resolve_user` → `libc::getpwnam`: `src/user_resolver.rs:30-50`. `MailboxConfig::owner_uid/owner_gid` resolve through it: `src/config.rs:345-368`. Config validation errors when the owner fails getpwnam: `src/config.rs:228, 423`.
- Default paths `DEFAULT_DATA_DIR="/var/lib/aimx"`, `DEFAULT_CONFIG_DIR="/etc/aimx"`, with env overrides `AIMX_CONFIG_DIR` / `AIMX_DATA_DIR`: `src/config.rs:38-39, 58, 65, 70`.
- `mailbox_handler.rs` imports `std::os::unix::fs::MetadataExt` unconditionally (`src/mailbox_handler.rs:38`) and checks directory uid/gid (`src/mailbox_handler.rs:466-473`). `mailbox_list_handler.rs` does the same (`src/mailbox_list_handler.rs:28, 129`).

**Clients of the UDS (CLI + MCP)**
- `aimx send` uses `tokio::net::UnixStream::connect` (`src/send.rs:240`) and refuses root via a cfg-gated `geteuid` (`src/send.rs:319-328, 417`). Its error hint is `systemctl status aimx` (`src/send.rs:456`).
- MCP server: `UnixStream::connect` at `src/mcp.rs:975, 1095, 1113, 1143, 1339, 1411`. "Start with 'sudo systemctl start aimx'" hints at `src/mcp.rs:588, 647, 691, 735, 820, 870, 1278, 1291`. Caller uid comes from `platform::current_euid()` (`src/mcp.rs:222`).
- `version_handler.rs` uses `UnixStream::connect` (`src/version_handler.rs:91`).

**setup.rs (installer/wizard)**
- `id` (`src/setup.rs:102`), `getpwnam` (`:123`), `useradd`/`adduser` for the system user (`:147-193`), shell `chown`/`chmod` (`:210-217`), `CATCHALL_HOME_DIR=/var/lib/aimx-catchall` (`:244`), `ensure_catchall_user` with a 0o700 home (`:256-280`).
- Service control through systemctl/rc-service (`src/setup.rs:320-367`). Service file install writes `/etc/systemd/system/aimx.service` + `systemctl daemon-reload/enable` or `/etc/init.d/aimx` + `rc-update` (`:441-500`). Uninstall at `:503-566`.
- `openssl` for the self-signed STARTTLS cert (`src/setup.rs:375-409`, spawn at `:384`). `geteuid` root check (`:429-431`). `ss -tlnp sport = :25` (`:433-435`). `dig` (`:672`). `curl` to the verifier (`:776`). `hostname -I` for server IPs (`:885`). Readiness probe dials `127.0.0.1:25` (`:574-592`).

**Other modules**
- dkim.rs: covered above. datadir_readme.rs: `PermissionsExt` 0o644 (`src/datadir_readme.rs:19-20`).
- doctor.rs: mode/uid checks (`src/doctor.rs:795-802, 990-1015`), chown hint (`:772`), hard-coded "/run/aimx/aimx.sock" message (`:139`), service state via `is_service_running` (`:115`).
- logs.rs: journalctl/OpenRC hint text (`src/logs.rs:56-57`). mailbox.rs: `systemctl restart` / `rc-service` restart hints (`src/mailbox.rs:943-947`) and getpwuid usage (`:428`). hooks.rs: systemctl hint (`src/hooks.rs:606`).
- agents_setup.rs: `geteuid` (`src/agents_setup.rs:494, 583`), `getpwnam` home lookup (`:597-613`), `OsStrExt` (`:613`), `PermissionsExt` (`:1601, 1609`). Destinations are `$HOME`/`$XDG_CONFIG_HOME` templates such as `$HOME/.claude/skills/aimx` (`:76-123`). agents_mcp.rs: `ExitStatusExt` (`src/agents_mcp.rs:133`).
- upgrade.rs: `PermissionsExt::set_mode(0o755)` (`src/upgrade.rs:529-531, 698`). Stops/starts the service and then polls readiness, which is the 127.0.0.1:25 probe (`src/upgrade.rs:298, 334, 346`). Renames over the installed binary with a `.prev` backup (`src/upgrade.rs:305-322`). The binary path comes from `std::env::current_exe()` (`src/setup.rs:410-419`), which is portable.
- uninstall.rs: root check (`src/uninstall.rs:6`), `uninstall_service_file` (`:39`), `/var/lib/aimx` default (`:90`).
- `nix` is used only in test code: `src/auth.rs:261` (test), `src/hook.rs:728` (test), `src/smtp/tests.rs:1604` (test).

**macOS-specific facts from crate sources**
- tokio's `UCred` peer-cred has a dedicated macOS implementation (`tokio-1.53.1/src/net/unix/ucred.rs:56-62, 288-294`). `pid()` is documented as implemented on "Linux, Android, OpenBSD, FreeBSD (since FreeBSD 13), NetBSD, NTO, iOS, macOS, …" (`ucred.rs:26-30`). — [tokio 1.53.1 crate](https://static.crates.io/crates/tokio/tokio-1.53.1.crate)
- tokio Unix-socket types are compiled only under `cfg(all(unix, feature = "net"))` (`tokio-1.53.1/src/macros/cfg.rs:374-380`). — [tokio 1.53.1 crate](https://static.crates.io/crates/tokio/tokio-1.53.1.crate)

### Inferences
- **macOS port, roughly a moderate lift.** The libc calls (geteuid, getpwnam, chown, setuid, setsid, kill), UDS, and peer_cred all exist on macOS. The fork+setuid hook fallback should work as is, because `detect_init_system` returns non-systemd on macOS. The required changes are:
  - a launchd plist generator in place of systemd/OpenRC;
  - `dscl`/`sysadminctl` in place of `useradd` (or drop per-user system accounts in desktop mode);
  - replacements for `ss`, `hostname -I`, and `journalctl` (`lsof`/`netstat`, getifaddrs, unified log or file logs);
  - `/run/aimx`, which does not exist on macOS (use `/var/run/aimx` or a per-user dir);
  - `/etc/ssl/aimx`.
- **Windows port, roughly a large lift.** Every UDS use needs a named-pipe or AF_UNIX equivalent. SO_PEERCRED has no direct equivalent; client identity must come from the pipe client token. Every chown/chmod becomes an ACL (or disappears in single-user mode). Hooks need a new executor (CreateProcess plus a Job Object for timeout and memory limits; there are no process groups or signals). The service needs Windows Service (SCM) or per-user autostart. Signals become `tokio::signal::windows::ctrl_*`.
- The `cfg(not(unix))` fallbacks that exist today are unsafe defaults for Windows. `current_euid()==0` makes every MCP caller "root" for `authorize()`, and DKIM keys are written without protection. They should become compile errors, or be routed through an explicit "single-user mode", rather than silently degrading.

### Gaps
- I did not compile for `x86_64-pc-windows-*` or `*-apple-darwin`. No cross toolchain (mingw or Apple SDK) was present, and aws-lc-sys needs a C cross toolchain to build. The claim that it does not compile on Windows rests on static evidence (unconditional `std::os::unix` imports at `src/ownership.rs:29`, `src/mailbox_handler.rs:38`, `src/mailbox_list_handler.rs:28`, `src/uds_authz.rs:20-21`; `tokio::signal::unix` at `src/serve.rs:630-633`; `libc::getpwnam` at `src/user_resolver.rs:46`). A `cargo check --target x86_64-pc-windows-msvc` on a Windows CI runner would give the exact error list.
- I could not verify whether macOS lets non-root processes bind port 25 (docs.rs and Apple docs unreachable). It matters less if relay mode removes the port 25 listener.

## Q2. Which Cargo.toml dependencies are Unix-only or problematic on Windows/macOS?

### Takeaway
Only two direct dependencies are inherently Unix-bound: `libc` (used for Unix-only symbols) and `nix` (an empty crate off Unix, and test-only in practice). tokio's `net`/`signal` features are cross-platform but expose different APIs per OS. `rustls` with `aws-lc-rs` builds on Windows but needs NASM (or the prebuilt-nasm opt-in) on x86-64. The networking/mail stack (hickory-resolver, mail-auth, lettre, ureq) supports Windows and macOS. The bigger dependency problem is the external binaries setup shells out to (systemctl, journalctl, rc-service, systemd-run, useradd, ss, dig, openssl, `hostname -I`), plus a release pipeline that ships only four Linux targets.

### Cited Findings
- Direct deps of concern: `libc = "0.2"` (`Cargo.toml:45`), `nix = { version = "0.31", features = ["fs","user"] }` (`Cargo.toml:46`), tokio with `net`+`signal` (`Cargo.toml:38`), rustls with `aws-lc-rs` (`Cargo.toml:40`), lettre `smtp-transport, tokio1, tokio1-rustls-tls` (`Cargo.toml:49`), `hickory-resolver = "0.26"` (`Cargo.toml:50`), `mail-auth = "0.10"` (`Cargo.toml:35`).
- nix starts with `#![cfg(unix)]`, so it compiles to nothing on Windows (`nix-0.31.3/src/lib.rs:45`), and its category is `os::unix-apis` (`nix-0.31.3/Cargo.toml:33`). — [nix 0.31.3 crate](https://static.crates.io/crates/nix/nix-0.31.3.crate). aimx uses it only in test code (`src/auth.rs:261`, `src/hook.rs:728`, `src/smtp/tests.rs:1604`, all test).
- tokio on Windows offers `tokio::net::windows::named_pipe` (`tokio-1.53.1/src/net/mod.rs:63`, `src/net/windows/mod.rs:3`) and `tokio::signal::windows::{ctrl_c, ctrl_break, ctrl_close, ctrl_shutdown, ctrl_logoff}` (`tokio-1.53.1/src/signal/windows.rs:46, 231, 259, 359, 459`). The named-pipe server builder exposes `reject_remote_clients` and `create_with_security_attributes_raw` (custom DACL) (`named_pipe.rs:2164, 2304`). No client process-id or impersonation accessor appears in the tokio source. — [tokio 1.53.1 crate](https://static.crates.io/crates/tokio/tokio-1.53.1.crate)
- aws-lc-rs: "CMake is **never** required" for non-FIPS builds (`aws-lc-rs-1.16.3/README.md:25`). "For Windows x86 and x86-64, NASM is required." The fallback is the `prebuilt-nasm` feature or `AWS_LC_SYS_PREBUILT_NASM=1` (`README.md:97-142`). — [aws-lc-rs 1.16.3 crate](https://static.crates.io/crates/aws-lc-rs/aws-lc-rs-1.16.3.crate). `ring 0.17.14` is also in the lockfile (`Cargo.lock:2337`).
- hickory-resolver's `system-config` feature pulls `ipconfig` on Windows and `system-configuration` on Apple, with `src/system_conf/{windows,apple,unix}.rs` present (`hickory-resolver-0.26.1/Cargo.toml:125-130, 360-364`). — [hickory-resolver 0.26.1 crate](https://static.crates.io/crates/hickory-resolver/hickory-resolver-0.26.1.crate). aimx ingest uses `mail_auth::MessageAuthenticator::new_system_conf()` with a Cloudflare fallback (`src/ingest.rs:771-774`).
- lettre 0.11.22 exposes `SmtpTransport::relay`, `starttls_relay`, `builder_dangerous`, `.credentials()`, `.authentication()`, `.port()`, `.tls()` (`lettre-0.11.22/src/transport/smtp/transport.rs:90, 114, 141, 295, 301, 324, 346`), with async equivalents in `async_transport.rs:135-440`. — [lettre 0.11.22 crate](https://static.crates.io/crates/lettre/lettre-0.11.22.crate)
- `sparkv 0.2.0` (dedup cache, same author) is described as an "Expirable in-memory key-value store with per-entry TTL and zero dependencies", so it is portable. — [sparkv 0.2.0 crate](https://static.crates.io/crates/sparkv/sparkv-0.2.0.crate)
- External binaries spawned in production code: `systemd-run` (`src/platform.rs:212`), `id`/`useradd`/`adduser`/`chown`/`chmod` (`src/setup.rs:102, 162, 183, 210, 217`), `openssl` (`:384`), `ss` (`:434`), `systemctl` (`:469-533`), `rc-update`/`rc-service` (`:489, 548, 553`), `dig` (`:672`), `curl` (`:776`), `hostname` (`:885`), plus journalctl/systemctl through `src/serve.rs:1200, 1229`.
- Release ships only `x86_64/aarch64-unknown-linux-{gnu,musl}`, built with `cross` on ubuntu-latest (`.github/workflows/release.yml:15-25, 58`). `install.sh` exits on any non-Linux `uname -s` with "AIMX is Linux-only" (`install.sh:361-366`). The version string embeds the build `TARGET` triple (`src/version.rs:27-30`), which `upgrade` uses to pick the tarball (`src/upgrade.rs:265-266`).

### Inferences
- Move `nix` to `[target.'cfg(unix)'.dev-dependencies]` and isolate `libc` calls behind a `platform::` façade.
- For Windows, add `windows-sys` (or the `windows` crate) for named-pipe client identity, ACLs, Job Objects, and SCM. Candidates are `windows-service` for SCM and `service-manager` as a cross-platform launchd/systemd/SCM abstraction. These were not verified in this session.
- Replace the shell-outs with in-process Rust where possible:
  - `dig` → hickory (already a dependency);
  - `openssl` → `rcgen` (already a dev-dependency) for the self-signed cert;
  - `curl` → `ureq` (already a dependency);
  - `hostname -I` → a getifaddrs crate or the verifier's echo of the public IP;
  - `ss` → a bind attempt.
- Add Windows (MSVC; set `AWS_LC_SYS_PREBUILT_NASM=1` or install NASM) and macOS (x86_64 + aarch64) targets to `release.yml`. Native runners would be needed (`cross` covers Linux only), plus a Windows/macOS installer path in place of `install.sh`.

### Gaps
- I did not check transitive dependencies (e.g., `dialoguer`, `console`, `colored`, `html2text`, `comrak`, `lol_html`) for Windows build quirks. They are generally described as cross-platform but were not verified here.

## Q3. Where is port 25 assumed, how pluggable is MailTransport, and can relay-delivered mail reuse the ingest path (and where must SPF/DKIM/DMARC verification live)?

### Takeaway
Port 25 is hard-wired in seven places: the inbound bind default, the CLI default, setup preflight (a hard fail), portcheck, the verifier, the service readiness probe (also used by upgrade), and outbound lettre `.port(25)`. Outbound is already behind a small `MailTransport` trait, so a smarthost or relay transport can be dropped in with little code. Inbound ingest is already decoupled from SMTP: `ingest::ingest_email(config, locks, rcpt, raw, received_from_ip, envelope_mail_from, HookMode)` takes raw RFC 5322 bytes plus envelope metadata. A tunnel client inside `aimx serve` can call it exactly as the SMTP session does. SPF/DMARC need the original connecting IP and MAIL FROM, which only the relay sees. DKIM, and therefore aimx's trust and hook gating, survives an unmodified relay hop.

### Cited Findings
**Port 25 assumptions**
- Inbound: `DEFAULT_BIND = "0.0.0.0:25"` (`src/serve.rs:248`), clap `--bind` default `"0.0.0.0:25"` (`src/cli.rs:301`), `TcpListener::bind(bind_addr)` (`src/serve.rs:563-565`). Help text "portcheck: Check port 25 connectivity" (`src/cli.rs:26`).
- Setup step 1 is a hard prerequisite. `check_port25_occupancy` via `ss` (`src/setup.rs:2454-2461`). It binds a temporary SMTP listener on `0.0.0.0:25` (`src/portcheck.rs:37-50`) and runs `run_port25_preflight`, which returns `Err("Port 25 checks failed…")` if outbound or inbound fails and prints `COMPATIBLE_PROVIDERS` (`src/setup.rs:2465-2470, 2762-2800`).
- Verifier coupling: `DEFAULT_VERIFY_HOST = "https://check.aimx.email"` and `DEFAULT_CHECK_SERVICE_SMTP_ADDR = "check.aimx.email:25"` (`src/setup.rs:626-628`). `derive_smtp_addr_from_verify_host` appends `:25` (`src/setup.rs:803-825`). Outbound check dials it (`:829`). Inbound check is `curl <verify_host>/probe` (`src/setup.rs:774-788, 878-879`).
- `aimx portcheck` command (`src/portcheck.rs:5-28, 148-200`).
- Readiness probe `127.0.0.1:25` (`src/setup.rs:574-592`), reused by `aimx upgrade` after restart (`src/upgrade.rs:346`).
- Outbound: `lettre::SmtpTransport::builder_dangerous(target).hello_name(config.domain).port(25).tls(Tls::Opportunistic(..))` (`src/transport.rs:264-272`). MX lookup with RFC 5321 A-record fallback via hickory (`src/mx.rs:4-49`). IPv4 pinning unless `enable_ipv6` (`src/transport.rs:40-53, 75-87, 111-123`). Per-MX iteration (`src/transport.rs:191-226`). 4xx/5xx classification into `Temp`/`Permanent` (`src/transport.rs:299-309`).

**MailTransport pluggability**
- Trait: `pub trait MailTransport { fn send(&self, sender: &str, recipient: &str, message: &[u8]) -> Result<String, TransportError>; }` with `TransportError::{Temp, Permanent}` (`src/transport.rs:15-38`).
- Two impls today: `LettreTransport` (`src/transport.rs:50-185`) and test-only `FileDropTransport` (`src/transport.rs:317-350`).
- Selection is hard-coded at daemon start: `AIMX_TEST_MAIL_DROP` env → FileDrop, else `LettreTransport::new(config.enable_ipv6, config.domain)` (`src/serve.rs:503-521`). It is stored as `Arc<dyn MailTransport + Send + Sync>` in `SendContext` (`src/send_handler.rs:59-67`).
- Call site: one synchronous `ctx.transport.send(&from_header, &recipient_bare, &signed)` per SEND (`src/send_handler.rs:326`). It runs after DKIM signing (`src/send_handler.rs:291-295`). The recipient is a single bare address extracted from `To:` (`src/send_handler.rs:213-236`). The trait is sync; LettreTransport bridges async DNS via `block_in_place` (`src/transport.rs:149-160`).
- `Config` has no outbound-mode or relay fields today: `domain, data_dir, dkim_selector, trust, trusted_senders, mailboxes, verify_host, signature, …` (`src/config.rs:124-160`).

**Inbound ingest path**
- SMTP session `deliver_message` loops over RCPTs and calls `tokio::task::spawn_blocking(|| ingest::ingest_email(&config, &locks, &rcpt, &data, peer.ip(), envelope, HookMode::Async))` (`src/smtp/session.rs:507-560`). It snapshots `ConfigHandle` per message (`:516`). The 250/451 partial-accept contract is at `:565-605`.
- `ingest_email` signature (`src/ingest.rs:129-137`). It must run on a blocking thread because of `blocking_lock()` (`src/ingest.rs:125-128`).
- RCPT-time recipient validation via `ingest::resolve_recipient_mailbox` (`src/smtp/session.rs:388`, `src/ingest.rs:80`).
- The manual stdin path (`aimx ingest`) passes sentinel IP `0.0.0.0` (`src/ingest.rs:91-110`).

**Auth verification**
- `verify_auth(raw, peer_ip, envelope_mail_from)` (`src/ingest.rs:777-823`) runs:
  - DKIM via `mail_auth` on the raw bytes (`:789-792`);
  - SPF with `(peer_ip, MAIL FROM)` (`:798-800, 872-897`);
  - DMARC combining both (`:802-812, 910-940`).
- `select_spf_inputs`: if the peer IP is unspecified it parses the IP out of the message's `Received:` header, and if MAIL FROM is empty it parses headers (`src/ingest.rs:826-852`). Results go into frontmatter `received_from_ip`, `dkim`, `spf`, `dmarc` (`src/ingest.rs:328-331, 437, 453-454`).
- Trust gating for hooks uses only `auth.dkim == "pass"` plus the sender allowlist (`src/trust.rs:79-115`, especially `:94-96`). It does not use SPF.

### Inferences
- **Outbound smarthost.** Add `SmarthostTransport` implementing `MailTransport`, using `lettre::SmtpTransport::starttls_relay(host)` / `relay(host)` with `.port(587|465)`, `.credentials()`, and required TLS. Wire it via a new `[outbound]` config block (e.g., `mode = "direct" | "smarthost" | "relay"`, `host`, `port`, `username`, password from a 0600 secrets file) at `src/serve.rs:503-521`. Estimated at about 150–250 lines plus tests.
- **Tunnel-based relay transport.** A WebSocket/HTTP2 `RelayTransport` fits the same trait. Because the trait is sync and called from an async handler, either keep the `block_in_place` pattern or make the trait `async` (a mechanical change at one call site, `src/send_handler.rs:326`).
- **Queueing.** The daemon has no retry queue; delivery is synchronous to the SEND caller. A relay can add store-and-forward. Consider carrying all envelope recipients (To/Cc/Bcc) per call rather than one.
- **Relay inbound.** Add a "relay client" task to `run_serve`, alongside or instead of the SMTP listener. It holds an outbound authenticated connection to the relay, receives `{mail_from, rcpts[], client_ip, helo, raw_bytes}` frames, and for each rcpt calls `ingest_email(..., client_ip, Some(mail_from), HookMode::Async)` in `spawn_blocking` with the same `ConfigHandle` and `MailboxLocks`. It acks success/failure per rcpt so the relay can retry, mirroring the 250/451 logic.
- **RCPT validation in relay mode.** It must happen at the relay (to avoid accept-then-bounce backscatter), so the node should push its mailbox list (`MAILBOX-LIST` already exists) or the relay should ask the node in real time over the tunnel.
- **Where SPF/DMARC verification should live.** The relay is the only party that sees the true connecting IP, HELO, and MAIL FROM. Two options:
  - (A) The relay forwards them as authenticated metadata and the node keeps doing verification. This needs a minimal code change: pass the relay-supplied IP as `received_from_ip`. Never fall back to the spoofable `Received:` parsing at `src/ingest.rs:840-844`.
  - (B) The relay verifies and stamps `Authentication-Results`/ARC, and the node trusts it only on the authenticated tunnel.
  
  (A) keeps aimx's "verification on your box" story. Either way DKIM can stay on the node as long as the relay only prepends headers and never rewrites the body or signed headers. Because hook trust depends only on DKIM (`src/trust.rs:94-96`), the trust model survives the relay.
- **Without port 25 or a public IP**, the TLS cert/STARTTLS machinery (`src/serve.rs:460-474`, `src/smtp/tls.rs`), the port 25 preflight, and `portcheck` become optional in relay mode.

### Gaps
- Not examined in depth: whether `send_handler` handles Cc/Bcc fan-out elsewhere. Only the `To:` extraction was seen at `src/send_handler.rs:213-236`.
- Not examined: how the failure-path `DeliveryStatus` is persisted (`src/send_handler.rs:340-390`).

## Q4. How does setup.rs handle DNS today, and what changes if MX points at a relay and outbound goes via a relay?

### Takeaway
Setup assumes the node *is* the MX, with a public IP. It detects the IP with `hostname -I`, then generates and verifies an A record, `MX 10 <domain>.`, `SPF v=spf1 ip4:<ip> -all`, DKIM TXT, and `DMARC p=reject`. Verification is strict: MX must equal the domain, SPF must literally contain the IP. In relay mode, MX must point at the relay, SPF must use `include:` (verification today only understands `ip4:`/`ip6:` tokens), the A record and public-IP detection are unnecessary, and DKIM can stay node-signed with the same TXT record.

### Cited Findings
- Server IP detection via `hostname -I` (`src/setup.rs:882-889`; parser at `:645-660`). Called once in setup (`src/setup.rs:2520`).
- `generate_dns_records(domain, server_ip, server_ipv6, dkim_value, selector)` (`src/setup.rs:1018-1069`) emits:
  - A (+ AAAA) → server IP (`:1031-1043`);
  - `MX` value `"10 {domain}."` (`:1045-1049`);
  - SPF `v=spf1 ip4:{ip} [ip6:{ipv6}] -all` (`:1025-1028`);
  - DKIM TXT at `{selector}._domainkey.{domain}` (`:1055-1059`);
  - DMARC `v=DMARC1; p=reject; rua=mailto:postmaster@{domain}` at `_dmarc.{domain}` (`:1060-1064`).
- Verification:
  - `verify_mx` fails with "MX record found but does not point to {domain}" (`src/setup.rs:1107-1123`);
  - `verify_a`/`verify_aaaa` expect the server IP (`:1127-1149`);
  - `spf_contains_ip` only matches `ip4:`/`+ip4:`/`ip6:`/`+ip6:` tokens (`:1151-1166`), used by `verify_spf` (`:1168-1186`);
  - `verify_dkim` compares the published `p=` with the local key (`:1188-1225`);
  - `verify_dmarc` (`:1227-1250`);
  - aggregate `verify_all_dns` (`:1252-1284`).
- DNS lookups shell out to `dig` with a resolver cascade (`src/setup.rs:661-735`, `dig_short_args` at `:954`). Retry loop in `run_setup` (`src/setup.rs:2583`).
- DKIM keypair generated at setup (`src/setup.rs:2533`). `aimx serve` compares the on-disk public key to DNS at startup, non-fatal (`src/serve.rs:494-498`). Signing happens in the daemon with an in-memory `Arc<RsaPrivateKey>` (`src/serve.rs:479-492`; `src/send_handler.rs:291-295`).
- Self-signed STARTTLS cert via openssl (`src/setup.rs:2617`, impl at `:375-409`).

### Inferences
- **Relay mode DNS:**
  - `MX 10 mx.<relay>.` (or a per-tenant `<domain>.mx.<relay>` CNAME-able host);
  - no A/AAAA requirement for the node;
  - SPF `v=spf1 include:_spf.<relay> -all` (plus ip4 if the node also sends direct);
  - DKIM TXT unchanged if the node keeps signing;
  - DMARC unchanged.
  
  Code changes: parameterize `generate_dns_records` by mode, teach `verify_mx` to accept the relay host, and teach `verify_spf` to accept `include:<relay>` (or resolve includes).
- **Keep DKIM signing on the node** (no key leaves the box; matches aimx's privacy positioning). Then DMARC alignment rides on DKIM `d=<domain>`. If the relay uses its own bounce/return-path domain, SPF alignment fails but DKIM alignment still passes DMARC. Optionally support a `bounce.<domain>` CNAME to the relay for SPF alignment.
- `DMARC p=reject` is a risky default during relay onboarding. Any DKIM breakage (e.g., relay header rewriting) would cause rejections. Consider `p=quarantine`/`none` until delivery is verified.
- In relay mode, setup step 1 (port 25 preflight), the TLS cert step, and `hostname -I` should be skipped. A new step replaces them: "register with relay / obtain tenant token / verify domain ownership" (e.g., a TXT challenge, reusing the `dig`→hickory verification plumbing).

### Gaps
- The exact relay DNS naming scheme is a design decision, not something in the codebase.

## Q5. What does services/verifier do today, and could it be extended into a relay service?

### Takeaway
The verifier is a tiny, stateless, single-file axum service: about 477 production lines plus about 930 lines of tests (43 tests). `/probe` connects back to the caller's IP on port 25 and does an EHLO. A toy port 25 responder answers only banner/EHLO/HELO/QUIT. It has no DATA, TLS, storage, tenancy, or authentication, so it is not a relay foundation beyond its deployment scaffolding: Caddy front, client-IP trust model, SSRF guard, concurrency limits. A relay is effectively a new service. aimx's own `src/smtp/` session engine and `ingest`/`mail-auth` code are the more reusable parts.

### Cited Findings
- Crate: `aimx-verifier` 0.1.0, edition 2021, deps axum 0.8, tokio full, serde, serde_json, tracing (`services/verifier/Cargo.toml`). Single source file `services/verifier/src/main.rs` (1,406 lines); `#[cfg(test)] mod tests` starts at `main.rs:478`. 43 `#[test]`/`#[tokio::test]` functions.
- Constants: `CLIENT_IP_HEADER = "X-AIMX-Client-IP"`, `SMTP_HOSTNAME = "check.aimx.email"`, timeouts and line limits, `SMTP_DEFAULT_MAX_CONCURRENT = 128` (`services/verifier/src/main.rs:17-32`).
- Routes `/`, `/health`, `/probe` (`main.rs:453-456`). HTTP binds `BIND_ADDR` default `127.0.0.1:3025` (`main.rs:459`).
- `/probe` resolves the client IP, trusting `X-AIMX-Client-IP` only when the TCP peer is loopback (`main.rs:75-99`). It guards against private/reserved targets (`is_blocked_target`, `main.rs:105-165`), then `check_port25_ehlo` connects to `SocketAddr::new(ip, 25)` (`main.rs:212-262`).
- SMTP responder on `SMTP_BIND_ADDR` default `0.0.0.0:25` (`main.rs:304-311`) with a semaphore (`main.rs:311-366`). The session handles only EHLO/HELO (250), QUIT (221), and everything else gets `500 Command not recognized` (`main.rs:397-447`).
- Deployment: docker-compose with `network_mode: host` for verifier + Caddy. Caddy terminates TLS for `{$DOMAIN:check.aimx.email}` and injects `X-AIMX-Client-IP {remote_host}` (`services/verifier/docker-compose.yml`, `services/verifier/Caddyfile`). README: "No MTA is required on the verifier server" (`services/verifier/README.md`).
- The FAQ positions the verifier as setup-only: "Nothing in the mail path depends on it" (`book/faq.md:204`). It is self-hostable via `verify_host` (`book/faq.md:208`).
- Reusable aimx inbound engine: `src/smtp/session.rs` (779 lines; full EHLO/MAIL/RCPT/DATA/STARTTLS/RSET/NOOP/QUIT state machine, size limits, per-RCPT handling), `src/smtp/mod.rs` (203 lines; accept loop, semaphore, timeouts, `src/smtp/mod.rs:110-140`), `src/smtp/tls.rs` (54 lines).

### Inferences
- A hosted relay needs these capabilities:
  - inbound MX with STARTTLS and real DATA handling (lift `src/smtp/session.rs`, replacing the `ingest_email` call at `session.rs:538` with "route to tenant queue");
  - a domain→tenant routing table with recipient validation;
  - durable store-and-forward for offline nodes;
  - an authenticated persistent node connection (WebSocket/HTTP2/QUIC, per-node token or mTLS);
  - outbound submission from nodes (SMTP AUTH on 587 or a tunnel frame) plus delivery from relay IPs with reputation/warm-up, rate limits, abuse controls, and bounce/DSN handling;
  - SPF/DKIM/DMARC evaluation or metadata pass-through (Q3);
  - multi-tenant signup/billing.
  
  This is a new `services/relay` crate. The verifier's Caddy/client-IP/SSRF patterns and the docker-compose host-network deployment carry over.
- The verifier can stay as is for direct-mode users. In relay mode setup no longer needs it, except for an optional "is port 25 open? then offer direct mode" check.

### Gaps
- Relay design specifics (protocol choice, queue store, IP reputation strategy) are outside the codebase and were not researched here.

## Q6. How does authorization work today, and what is the single-user desktop equivalent?

### Takeaway
Authorization is one pure predicate, `auth::authorize(caller_uid, action, mailbox)`: uid 0 bypasses everything, otherwise the caller uid must equal the mailbox owner's getpwnam-resolved uid. The daemon gets `caller_uid` from SO_PEERCRED on the UDS. MCP/CLI *reads* bypass the daemon and rely on filesystem permissions (owner-chowned 0700 dirs). Hooks run as the owner via setuid. Everything is keyed on Unix uids. On a single-user desktop the natural equivalent is "the daemon runs as the logged-in user, the IPC endpoint is ACL'd to that user, every mailbox is implicitly owned by that user". That collapses authz to "caller == daemon user" and removes chown, setuid, and root. The current `cfg(not(unix))` behavior already treats every caller as uid 0, which is unsafe by accident.

### Cited Findings
- `authorize`:
  - `caller_uid == 0` → Ok (`src/auth.rs:201-203`);
  - `SystemCommand` → `NotRoot` (`:206`);
  - `MailboxCreate{owner_uid}` requires `caller_uid == owner_uid`, else `OwnerMismatch` (`:207-224`);
  - `MailboxDelete/MailboxRead/MailboxSendAs/MarkReadWrite/HookCrud` require `mb.owner_uid() == caller_uid`; an unresolvable owner maps to `NoSuchMailbox` (`:226-241`).
  
  The module is documented as pure logic shared by UDS handlers and CLI gating (`src/auth.rs:1-13`).
- The caller identity comes from `stream.peer_cred()` at accept (`src/uds_authz.rs:182-187`; `src/serve.rs:757-763`). Username via `getpwuid` (`src/uds_authz.rs:150-176`).
- `require_mailbox_owner_or_root` for SEND/MARK/HOOK verbs (`src/uds_authz.rs:207-230`). The module doc table still says MAILBOX-CREATE/DELETE are "root only" (`src/uds_authz.rs:11`), while CLAUDE.md and mailbox_handler describe owner-bound create. Non-root create synthesizes the owner from the peer uid via `peer_username` (`src/mailbox_handler.rs:126-135`). This is doc drift.
- `aimx send` refuses root (`src/send.rs:417`). `aimx setup`/`uninstall` require root (`src/setup.rs:2444-2448`, `src/uninstall.rs:6`).
- MCP reads email files directly from paths returned by the daemon's `MAILBOX-LIST` (`std::fs::read_to_string`/`read_dir` at `src/mcp.rs:366, 496, 1552, 1660`). The comment says there is no UDS read verb because "aimx mcp and the CLI inspect the filesystem directly" (`src/auth.rs:36-39`).
- MCP caller uid = `platform::current_euid()` (`src/mcp.rs:222`), which returns `0` on non-Unix (`src/platform.rs:35-45`).
- Per-mailbox OS isolation is a headline claim. "They can't read each other's mail … enforced by the operating system, not by aimx" (`README.md:193`). The owner is "Linux user that owns the mailbox storage and runs hooks" (`README.md:168`). The FAQ says MCP authz derives from "which Linux user is running `aimx mcp`" (`book/faq.md:180`).
- Catchall storage uses a reserved `aimx-catchall` system user created via useradd (`src/setup.rs:244-280`).

### Inferences
- **Proposed `DeploymentMode` enum, chosen at setup and stored in config:**
  - **Multi-user server mode (today's model; Linux, optionally macOS).**
    - Root daemon. Per-mailbox Unix owner. SO_PEERCRED/getpeereid authz.
    - chown 0700 storage. setuid + systemd-run hooks.
    - Port 25 direct or relay.
  - **Single-user desktop mode (macOS/Windows/Linux desktop).**
    - Hosting: the daemon runs as the logged-in user (launchd LaunchAgent; Windows per-user Task Scheduler/Run-key or a per-user service; `systemd --user` on Linux). Paths are per-user: `~/Library/Application Support/aimx`, `%LOCALAPPDATA%\aimx`, `$XDG_DATA_HOME/aimx`.
    - IPC endpoint is restricted to the user:
      - Unix: a UDS at 0600 in `$XDG_RUNTIME_DIR` or the app-support dir; peer uid must equal the daemon euid.
      - Windows: named pipe `\\.\pipe\aimx-<user-SID>` created with `reject_remote_clients(true)` and a DACL granting only the user SID via `create_with_security_attributes_raw`. Optionally verify the client via impersonation/`GetNamedPipeClientProcessId`.
    - Mailboxes: no `owner` field required (implicit = current user).
    - Removed: chown/chmod, setuid, the root bypass, and the `aimx-catchall` system user.
    - Hooks run as the same user under a timeout/memory wrapper (Windows Job Object; Unix setsid + rlimits).
    - Outbound/inbound via relay or smarthost, since port 25 is unavailable on residential networks.
- **Implementation seam.** Replace `caller_uid: u32` with a `Principal` (enum `{Root, Uid(u32), Sid(String), DesktopUser}`) produced by a `PeerIdentity` trait per IPC transport. `authorize` keeps its shape: in desktop mode "owner" comparisons become `principal == daemon_principal`.
- **Fix the non-Unix fallback.** `current_euid()` returning 0 should be replaced. As it stands, the MCP pre-flight `authorize` passes everything on Windows.
- **Relay mode also enables a rootless Linux server mode.** It removes the need to bind port 25 (the reason the daemon runs as root; cf. the test assertion "User=root remains; the daemon still binds port 25" at `src/serve.rs:1400` (test)), so `systemd --user` or a `DynamicUser` unit becomes possible.

### Gaps
- Windows named-pipe client identity APIs (`GetNamedPipeClientProcessId`, `ImpersonateNamedPipeClient`) could not be confirmed from Microsoft docs (learn.microsoft.com unreachable from the sandbox). tokio's named-pipe source has no such accessor, so `windows-sys` FFI would be needed.
- Not checked: whether Windows AF_UNIX sockets offer a peer-identity ioctl.

## Q7. How large is the codebase, how many tests, and which CI jobs exist?

### Takeaway
The main crate has about 53k lines of Rust across 54 files in `src/` (about 27k outside `mod tests` blocks; upper bound) and 10k lines of integration tests. There are about 1,360 test functions in total (1,161 inline + 157 integration + 43 verifier). CI runs only on `ubuntu-latest` (4 jobs + a release matrix of 4 Linux targets), with no macOS or Windows runners. The platform-sensitive modules (setup 6.6k, serve 2.5k, platform 0.7k, uds_authz/auth 1.2k, ownership/user_resolver 0.4k, mcp 2.7k, agents_setup 4.6k) together are about 19k lines, roughly 35% of `src/`.

### Cited Findings
- `wc -l src/*.rs src/smtp/*.rs` = 53,191 total. Largest files:

  | File | Lines |
  |---|---|
  | `setup.rs` | 6,556 |
  | `agents_setup.rs` | 4,550 |
  | `ingest.rs` | 3,266 |
  | `mcp.rs` | 2,738 |
  | `serve.rs` | 2,513 |
  | `wire_assembly.rs` | 1,945 |
  | `send_handler.rs` | 1,943 |
  | `send_protocol.rs` | 1,890 |
  | `mailbox_handler.rs` | 1,883 |
  | `smtp/tests.rs` | 1,690 |
  | `config.rs` | 1,629 |
  | `hook.rs` | 1,408 |
  | `upgrade.rs` | 1,356 |
  | `send.rs` | 1,348 |
  | `doctor.rs` | 1,339 |
  | `mailbox.rs` | 1,107 |
  | `frontmatter.rs` | 1,048 |
  | `state_handler.rs` | 1,047 |
  | `hook_handler.rs` | 1,019 |
  | `agents_tui.rs` | 998 |
  | `smtp/session.rs` | 779 |
  | `release.rs` | 771 |
  | `platform.rs` | 721 |
  | `dkim.rs` | 720 |
  | `hooks.rs` | 722 |
  | `auth.rs` | 639 |
  | `uds_authz.rs` | 557 |
  | `transport.rs` | 521 |
  | `portcheck.rs` | 453 |
  | `ownership.rs` | 268 |
  | `uninstall.rs` | 257 |
  | `logs.rs` | 221 |
  | `smtp/mod.rs` | 203 |
  | `user_resolver.rs` | 164 |
  | `mx.rs` | 123 |
  | `smtp/tls.rs` | 54 |

  Source: `wc -l` in repo.
- Lines before each file's `mod tests` sum to about 27,470. This is an upper bound: `doctor.rs`, `mcp.rs`, `send_protocol.rs`, `smtp/session.rs`, and `setup.rs` use differently named test modules. For example, `setup.rs` tests start at `src/setup.rs:2804` (`pub(crate) mod tests`), so setup's production code is about 2.8k lines.
- Integration tests: `tests/integration.rs` 7,463, `uds_authz.rs` 976, `e2e_agent_flow.rs` 661, `release_integration.rs` 378, `isolation.rs` 282, `mailbox_isolation.rs` 261, `hooks_list_filter.rs` 233 (10,254 total). Plus `tests/install_sh.sh`.
- Test function counts (`#[test]`/`#[tokio::test]`): 1,161 in `src/`, 157 in `tests/`, 43 in `services/verifier/src/`. 50 `#[ignore]` attributes and 5 `#[serial]` in `src/` + `tests/`.
- CI `.github/workflows/ci.yml`:
  - `core-tests` (ubuntu-latest: fmt, clippy `--all-targets -D warnings`, `cargo test`, author metadata) (`ci.yml:13-38`);
  - `mailbox-dir-perms-isolation` (ubuntu-latest; builds test binaries, then runs `mailbox_isolation`, `uds_authz`, `hooks_list_filter`, and the integration MCP smoke under sudo with real test users, then tears them down) (`ci.yml:53-131`);
  - `docs-build` (`ci.yml:133`);
  - `verifier-tests` (fmt/clippy/test in `services/verifier`) (`ci.yml:161-184`).
- `release.yml`: `build` matrix of 4 Linux targets via `cross` (`release.yml:15-58`), then `release` (`release.yml:105`). `site.yml`: `build` + `deploy` for the website (`site.yml:27, 81`).
- Docs: `book/*.md` total 3,778 lines. Hard-coded mentions:

  | Pattern | book/ | agents/ bundles |
  |---|---|---|
  | `/var/lib/aimx` | 41 | 17 |
  | `/etc/aimx` | 45 | 11 |
  | `/run/aimx` | 19 | 1 |
  | `systemctl`/`journalctl`/`OpenRC` | 49 | 7 |
  | `port 25` | 44 | 0 |
  | `sudo` | 89 | 16 |

  Source: grep counts. Agent bundles live under `agents/{claude-code,codex,common,gemini,goose,hermes,nanoclaw,openclaw,opencode}`.

### Inferences
Rough effort by workstream (engineer-weeks, assuming one engineer familiar with the code; these are judgement calls, not measurements):
1. **Platform façade + macOS server/desktop port** (launchd, paths, user creation or desktop mode, replace ss/hostname/dig/openssl shell-outs, macOS CI runner): ~3–5 weeks.
2. **Windows port** (named-pipe IPC + identity, ACLs or desktop-only mode, Job Object hook runner, SCM/autostart, signals, installer, Windows CI with NASM): ~6–10 weeks.
3. **Smarthost outbound transport + config + setup DNS variants:** ~1–2 weeks.
4. **Relay client in `aimx serve`** (tunnel, ingest wiring, mailbox sync, SPF metadata): ~2–4 weeks.
5. **Hosted relay service** (new crate; MX ingress, queue, tenancy, outbound IP reputation, abuse, billing): ~8–16+ weeks, plus ongoing ops.
6. **Docs/agents bundle rewrite for multi-platform/multi-mode:** ~1–2 weeks.

The existing ~1,360 tests are a strong regression net, but the root-gated isolation suite (`ci.yml:53-131`) is Linux-specific and would need desktop-mode counterparts.

### Gaps
- Exact production-vs-test line split for files with non-standard test module names was not computed.
- What the 50 `#[ignore]` tests are ignored for (e.g., root or network) was not inspected individually.

## Q8. What do the book/, README, and website currently claim (requirements, positioning)?

### Takeaway
Every public surface positions aimx as Linux-only, VPS-with-port-25, "no relay" software. A relay/smarthost mode and macOS/Windows desktop support would directly contradict current marketing copy ("no third-party relays involved", "No SaaS relay", "Linux-only by policy"). The messaging needs a deliberate repositioning, e.g., "direct mode when you have port 25; relay mode otherwise; DKIM keys and mail storage stay on your machine either way".

### Cited Findings
- README:
  - badge "Platform: Linux" (`README.md:25`);
  - "Runs on the same VPS you already provisioned for your agents" (`README.md:32`);
  - Requirements: "A Linux server with port 25 open (inbound and outbound)" (`README.md:57`);
  - mailbox `owner` = "Linux user" (`README.md:168`), with per-mailbox OS isolation (`README.md:193, 208`);
  - "designed for a single operator … single domain" (`README.md:277`);
  - FAQ "Why does AIMX need port 25 open? … AIMX speaks SMTP directly to other mail servers, no third-party relays involved" (`README.md:294-295`);
  - "Other mail servers will only deliver to you on 25" (`README.md:298`);
  - "Home ISPs typically block port 25" (`README.md:301`);
  - comparison table includes "SaaS relays (SendGrid / Postmark)" as an alternative category (`README.md:251`).
- book:
  - "OS: Linux (x86_64 or aarch64, glibc or musl)" and "A server (usually a VPS) with port 25 open" (`book/getting-started.md:7-8`);
  - "AIMX is Linux-only … Non-Linux platforms are refused … Linux-only by policy" (`book/installation.md:13, 22, 169`);
  - "A Linux server with port 25 open inbound **and** outbound" (`book/setup.md:11`);
  - FAQ: port 25 rationale (`book/faq.md:5-15`); "AIMX *is* the MTA, not a client of one, so submission ports do not apply" (`book/faq.md:49`); Docker/NAT requires port-forwarding 25 and "PROXY protocol is not supported" (`book/faq.md:51-53`); "the default `/run/aimx/aimx.sock` is hard-coded today. A second instance needs a source patch" (`book/faq.md:57`); "no SMTP AUTH … no submission port on 587" (`book/faq.md:158`);
  - architecture diagram alt text "sender → port 25 → aimx serve …" (`book/index.md:10`).
- Website `website/public/index.html`:
  - JSON-LD `"operatingSystem": "Linux"` (`:37`);
  - port 25 FAQ entries (`:53-64, 556-565`);
  - install snippet comment "# On VPS with open port 25" (`:189`);
  - "No SaaS relay, no OAuth, no Gmail scanning" (`:206`);
  - "No runtime deps, no database, no relay" (`:211`);
  - "Direct MTA-to-MTA delivery. No relay, no submission port, no SaaS in the middle" (`:392`);
  - "No SaaS relay" (`:448`).

### Inferences
- Copy that would need rewriting includes the README requirements and FAQ, `book/getting-started.md`, `installation.md`, `setup.md`, and `faq.md` (port 25, Docker/NAT, submission ports, single UDS path), `book/index.md`, and the website hero/FAQ/feature list.
- The privacy differentiator can be preserved, and should be made explicit: DKIM signing and storage stay local, and the relay only sees transit bytes. This only holds if node-side DKIM signing and node-side verification (Option A in Q3) are kept.
- The "single operator / single domain" positioning (`README.md:277`) fits a single-user desktop mode naturally. The per-mailbox Linux-user isolation story (`README.md:193`) would become a server-mode-only feature.

### Gaps
- Not reviewed: `book/security.md`, `book/troubleshooting.md`, or `website/public/llms.txt` line by line. Grep counts indicate many more Linux-path references.

## Q9. Where should the abstraction seams go?

### Takeaway
Seven seams cover nearly all of the work:
1. `Platform`/`ServiceManager` (init system, service lifecycle, logs);
2. `Ipc` + `PeerIdentity` (UDS vs named pipe; uid vs SID vs single-user);
3. `Principal`-based `authorize`;
4. `OwnershipPolicy` (chown/chmod vs ACL vs no-op);
5. `HookExecutor` (systemd-run / fork+setuid / Windows Job Object / same-user);
6. `MailTransport` variants (Direct/Smarthost/Relay; the trait already exists);
7. `InboundSource` (SMTP listener vs relay tunnel), both feeding the existing `ingest_email`.

Paths and setup steps then select by `DeploymentMode` × `MailRoute`.

### Cited Findings
- Already-existing seams to extend:
  - the `SystemOps` and `NetworkOps` traits in setup (`src/setup.rs:17-102`, `src/setup.rs:286-302`), which already cover service start/stop/restart, root check, port 25 occupancy, service-file install, user creation, and logs;
  - `MailTransport` (`src/transport.rs:35-38`);
  - pure `authorize` (`src/auth.rs:196`);
  - transport-agnostic `AIMX/1` codec over generic `AsyncRead`/`AsyncWrite` (`src/send_protocol.rs:95, 421-487, 1165-1339`);
  - the `Caller` struct decoupled from the socket type except in its constructors (`src/uds_authz.rs:31-60, 182`);
  - the `ingest_email` entry point taking raw bytes plus metadata (`src/ingest.rs:129-137`);
  - `config_dir()`/`dkim_dir()`/`runtime_dir()` with env overrides (`src/config.rs:58-72`, `src/serve.rs:261-273`).
- Points where concrete OS types leak through those seams:
  - `run_send_listener(listener: tokio::net::UnixListener…)` and `handle_uds_connection(stream: tokio::net::UnixStream…)` (`src/serve.rs:745-750, 810-835`);
  - UDS connects in clients (`src/send.rs:240`; `src/mcp.rs:975, 1095, 1113, 1143, 1339, 1411`; `src/version_handler.rs:91`);
  - `chown_as_owner` called directly from 5 modules (Q1);
  - `spawn_sandboxed` choosing via `detect_init_system` (`src/platform.rs:162-176`);
  - transport selection by env var (`src/serve.rs:503-521`);
  - the SMTP listener as the only inbound source (`src/serve.rs:563, 684`).

### Inferences
Proposed module layout:
- `src/platform/{mod.rs, linux.rs, macos.rs, windows.rs}`: `ServiceManager` trait (systemd, OpenRC, launchd, SCM, user-session), `paths()` (config, data, runtime, logs per OS × mode), `is_privileged()`.
- `src/ipc/{mod.rs, unix.rs, windows.rs}`: `IpcListener`/`IpcStream` (UDS or named pipe) yielding `(stream, Principal)`. Clients use `ipc::connect()`. `serve.rs`, `send.rs`, `mcp.rs`, and `version_handler.rs` switch to it. `Caller` becomes `Principal`.
- `auth.rs`: `authorize(principal, action, mailbox, mode)`. In `DesktopSingleUser` mode an owner check becomes "principal is the daemon user".
- `ownership.rs` → `StoragePolicy` trait: `UnixOwnerPolicy` (chown/chmod), `WindowsAclPolicy`, `NoopPolicy` (desktop).
- `platform::hooks` → `HookExecutor` trait: `SystemdRun`, `ForkSetuid`, `SameUser` (Unix setsid + rlimit), `WindowsJob`.
- `transport.rs`: `DirectMxTransport` (current), `SmarthostTransport` (lettre relay + AUTH + required TLS), `RelayTunnelTransport`. Config-driven `[outbound] mode`.
- `inbound/`: `SmtpListenerSource` (current `smtp/`) and `RelayTunnelSource`. Both call `ingest_email` in `spawn_blocking` with shared `ConfigHandle`/`MailboxLocks`, and pass authoritative `client_ip`/`mail_from` for SPF.
- `setup.rs`: branch the wizard by `MailRoute::{DirectPort25, Relay, SmarthostOnly}`. Skip the port 25 preflight, TLS cert, and `hostname -I` in relay mode. Generate relay DNS records (MX → relay, SPF include). Replace `dig`/`openssl`/`curl`/`ss`/`hostname` shell-outs with in-process Rust.
- CI: add `macos-latest` and `windows-latest` jobs (at least `cargo check` + unit tests). Add release targets `x86_64/aarch64-apple-darwin` and `x86_64-pc-windows-msvc`, plus a non-`install.sh` installer (Homebrew tap, MSI/winget, or a PowerShell script).

### Gaps
- None in the code. The open questions are product choices: whether Windows multi-user server mode is in scope at all (recommend desktop-only on Windows), and which tunnel protocol the relay uses.
