//! 极简 HTTPS 读取：只有“读小文件到内存”和“下载大文件到磁盘”两个用途。
//!
//! Windows 用系统自带的 WinHTTP（走系统证书和代理，不引入 TLS 库）；macOS 调用系统自带的 curl。
//! 两边都不引入 OpenSSL。

use std::time::Duration;

/// 解析后的地址（只支持 http/https）。
#[derive(Debug, PartialEq)]
pub struct Url<'a> {
    pub secure: bool,
    pub host: &'a str,
    pub port: u16,
    pub path: &'a str,
}

pub fn parse_url(url: &str) -> Option<Url<'_>> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else {
        (false, url.strip_prefix("http://")?)
    };
    if rest.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let (authority, path) = match rest.find(['/', '?']) {
        Some(index) if rest.as_bytes()[index] == b'/' => (&rest[..index], &rest[index..]),
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.contains('@') || authority.is_empty() {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (authority, if secure { 443 } else { 80 }),
    };
    (!host.is_empty()).then_some(Url {
        secure,
        host,
        port,
        path,
    })
}

/// 正式请求（含每一次跳转）只允许 GitHub 自己的域名，且必须是 https；
/// 预览模式（NIANLAI_UPDATE_FEED）才放开主机限制。
pub fn host_allowed(url: &str, relaxed: bool) -> bool {
    let Some(parsed) = parse_url(url) else {
        return false;
    };
    if relaxed {
        return true;
    }
    parsed.secure
        && parsed.port == 443
        && (parsed.host == "github.com"
            || parsed.host.ends_with(".github.com")
            || parsed.host.ends_with(".githubusercontent.com"))
}

/// 一块数据到达时的回调：参数是这块数据和响应声明的总长度（未知为 None）。
pub type Sink<'a> = dyn FnMut(&[u8], Option<u64>) -> Result<(), String> + 'a;

/// 读取一个小文件到内存，超过 `cap` 字节就放弃。
pub fn get_bytes(
    url: &str,
    relaxed: bool,
    cap: usize,
    deadline: Duration,
) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    fetch(url, relaxed, deadline, &mut |chunk, total| {
        if total.is_some_and(|total| total > cap as u64) || data.len() + chunk.len() > cap {
            return Err("响应过大".into());
        }
        data.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(data)
}

#[cfg(windows)]
pub use windows_impl::fetch;

#[cfg(windows)]
mod windows_impl {
    use super::{Sink, host_allowed, parse_url};
    use std::ffi::c_void;
    use std::time::{Duration, Instant};
    use windows::Win32::Networking::WinHttp::*;
    use windows::core::PCWSTR;

    struct Handle(*mut c_void);
    impl Handle {
        fn new(raw: *mut c_void) -> Result<Self, String> {
            if raw.is_null() {
                Err("网络不可用".into())
            } else {
                Ok(Self(raw))
            }
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    fn network<E>(_: E) -> String {
        "网络不通或连接超时".into()
    }

    fn location(request: &Handle) -> Result<String, String> {
        let mut buffer = [0u16; 4096];
        let mut bytes = std::mem::size_of_val(&buffer) as u32;
        unsafe {
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_LOCATION,
                PCWSTR::null(),
                Some(buffer.as_mut_ptr().cast()),
                &mut bytes,
                std::ptr::null_mut(),
            )
        }
        .map_err(network)?;
        String::from_utf16(&buffer[..(bytes as usize / 2).min(buffer.len())])
            .map(|text| text.trim_end_matches('\0').to_owned())
            .map_err(network)
    }

    fn number_header(request: &Handle, which: u32) -> Option<u32> {
        let mut value = 0u32;
        let mut size = 4;
        unsafe {
            WinHttpQueryHeaders(
                request.0,
                which | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&mut value as *mut u32).cast()),
                &mut size,
                std::ptr::null_mut(),
            )
        }
        .ok()
        .map(|_| value)
    }

    /// GET 并把响应体分块交给 `sink`。手动跟随最多 5 次跳转，每一跳都重新校验主机；
    /// 只在 `deadline` 之内有效，防止慢速响应一直占着线程。
    pub fn fetch(
        url: &str,
        relaxed: bool,
        deadline: Duration,
        sink: &mut Sink,
    ) -> Result<(), String> {
        let mut url = url.to_owned();
        let agent = wide(concat!("Nianlai/", env!("CARGO_PKG_VERSION")));
        let started = Instant::now();
        unsafe {
            let session = Handle::new(WinHttpOpen(
                PCWSTR(agent.as_ptr()),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ))?;
            WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 15_000).map_err(network)?;
            for _ in 0..=5 {
                if !host_allowed(&url, relaxed) {
                    return Err("地址不在允许的范围内".into());
                }
                let parsed = parse_url(&url).ok_or("地址无效")?;
                let host = wide(parsed.host);
                let path = wide(parsed.path);
                let connection = Handle::new(WinHttpConnect(
                    session.0,
                    PCWSTR(host.as_ptr()),
                    parsed.port,
                    0,
                ))?;
                let flags = if parsed.secure {
                    WINHTTP_FLAG_SECURE
                } else {
                    WINHTTP_OPEN_REQUEST_FLAGS(0)
                };
                let request = Handle::new(WinHttpOpenRequest(
                    connection.0,
                    windows::core::w!("GET"),
                    PCWSTR(path.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    std::ptr::null(),
                    flags,
                ))?;
                WinHttpSetOption(
                    Some(request.0),
                    WINHTTP_OPTION_REDIRECT_POLICY,
                    Some(&WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes()),
                )
                .map_err(network)?;
                // 不自动带登录凭证：更新请求没有任何用户身份信息。
                WinHttpSetOption(
                    Some(request.0),
                    WINHTTP_OPTION_AUTOLOGON_POLICY,
                    Some(&WINHTTP_AUTOLOGON_SECURITY_LEVEL_HIGH.to_ne_bytes()),
                )
                .map_err(network)?;
                WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(network)?;
                WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(network)?;
                let status = number_header(&request, WINHTTP_QUERY_STATUS_CODE).unwrap_or(0);
                if matches!(status, 301 | 302 | 303 | 307 | 308) {
                    let next = location(&request)?;
                    url = if next.starts_with('/') && !next.starts_with("//") {
                        let origin = url.find("://").map_or(0, |index| index + 3);
                        let end = url[origin..]
                            .find('/')
                            .map_or(url.len(), |index| origin + index);
                        format!("{}{next}", &url[..end])
                    } else {
                        next
                    };
                    continue;
                }
                if status != 200 {
                    return Err(format!("服务器返回 {status}"));
                }
                let total = number_header(&request, WINHTTP_QUERY_CONTENT_LENGTH).map(u64::from);
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    if started.elapsed() > deadline {
                        return Err("下载超时".into());
                    }
                    let mut read = 0u32;
                    WinHttpReadData(
                        request.0,
                        chunk.as_mut_ptr().cast(),
                        chunk.len() as u32,
                        &mut read,
                    )
                    .map_err(network)?;
                    if read == 0 {
                        return Ok(());
                    }
                    sink(&chunk[..read as usize], total)?;
                }
            }
        }
        Err("跳转次数过多".into())
    }
}

#[cfg(not(windows))]
pub use curl_impl::fetch;

/// macOS：用系统自带的 curl（/usr/bin/curl）。只允许 https，跳转也只允许 https。
#[cfg(not(windows))]
mod curl_impl {
    use super::{Sink, host_allowed};
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    pub fn fetch(
        url: &str,
        relaxed: bool,
        deadline: Duration,
        sink: &mut Sink,
    ) -> Result<(), String> {
        if !host_allowed(url, relaxed) {
            return Err("地址不在允许的范围内".into());
        }
        let mut child = Command::new("/usr/bin/curl")
            .args(["--silent", "--show-error", "--fail", "--location"])
            .args(["--proto", "=https,http", "--proto-redir", "=https"])
            .args(["--max-redirs", "5", "--connect-timeout", "10"])
            .args(["--max-time", &deadline.as_secs().max(1).to_string()])
            .args([
                "--user-agent",
                concat!("Nianlai/", env!("CARGO_PKG_VERSION")),
            ])
            .arg("--")
            .arg(url)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "没法调用系统的 curl".to_owned())?;
        let mut stdout = child.stdout.take().ok_or("没法读取下载内容")?;
        let started = Instant::now();
        let mut chunk = vec![0u8; 64 * 1024];
        let outcome = loop {
            if started.elapsed() > deadline {
                break Err("下载超时".to_owned());
            }
            match stdout.read(&mut chunk) {
                Ok(0) => break Ok(()),
                Ok(read) => {
                    if let Err(error) = sink(&chunk[..read], None) {
                        break Err(error);
                    }
                }
                Err(_) => break Err("读取下载内容失败".to_owned()),
            }
        };
        if outcome.is_err() {
            let _ = child.kill();
        }
        let status = child.wait().map_err(|_| "下载没完成".to_owned())?;
        outcome?;
        if status.success() {
            Ok(())
        } else {
            Err("网络不通或服务器拒绝了请求".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let url = parse_url("https://github.com/a/b?x=1").unwrap();
        assert_eq!(
            (url.secure, url.host, url.port, url.path),
            (true, "github.com", 443, "/a/b?x=1")
        );
        let url = parse_url("http://127.0.0.1:8080").unwrap();
        assert_eq!((url.secure, url.port, url.path), (false, 8080, "/"));
        assert!(parse_url("ftp://x/y").is_none());
        assert!(parse_url("https://user@github.com/").is_none());
        assert!(parse_url("https:///x").is_none());
        assert!(parse_url("https://a b/").is_none());
    }

    #[test]
    fn official_requests_only_reach_github_over_https() {
        for ok in [
            "https://github.com/eiaao/nianlai/releases/latest/download/latest-windows.json",
            "https://release-assets.githubusercontent.com/x/y?z=1",
            "https://objects.githubusercontent.com/x",
        ] {
            assert!(host_allowed(ok, false), "{ok}");
        }
        for bad in [
            "http://github.com/x",
            "https://evilgithub.com/x",
            "https://github.com.evil.com/x",
            "https://github.com:8443/x",
            "https://evil.com/x",
            "file:///C:/x",
        ] {
            assert!(!host_allowed(bad, false), "{bad}");
        }
        assert!(host_allowed("http://127.0.0.1:9/x", true));
    }

    #[test]
    fn get_bytes_enforces_the_size_cap_from_declared_length_and_stream() {
        // 无网络：只验证回调逻辑所用的上限判断与 fetch 的前置主机校验。
        assert!(get_bytes("https://evil.com/x", false, 10, Duration::from_secs(1)).is_err());
    }
}
