//! Updates from the project's GitHub releases: whether a newer version is out, and putting its
//! exe in place of the running one.

use crate::config;
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

/// Where the releases are published.
pub const REPO: &str = "Maximus657/ableton-skin-lab";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The exe attached to every release.
const ASSET: &str = "AbletonSkinLab.exe";

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub notes: String,
    url: String,
    size: u64,
}

/// The newest release, when it is newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    let body = get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let version = v["tag_name"].as_str().ok_or("the release has no version")?.trim_start_matches(['v', 'V']).to_string();
    if !newer(&version, VERSION) {
        return Ok(None);
    }
    let asset = v["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(ASSET))))
        .ok_or_else(|| format!("release {version} has no {ASSET}"))?;
    Ok(Some(Release {
        notes: v["body"].as_str().unwrap_or_default().to_string(),
        url: asset["browser_download_url"].as_str().ok_or("no download link")?.to_string(),
        size: asset["size"].as_u64().unwrap_or(0),
        version,
    }))
}

/// Whether version `a` ("1.2.0") is newer than `b`.
fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| {
        let mut n = [0u64; 3];
        for (i, part) in v.split('.').take(3).enumerate() {
            n[i] = part.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0);
        }
        n
    };
    parse(a) > parse(b)
}

/// Downloads the release and swaps it in for the running exe, whose path it returns. Windows lets
/// a running exe be renamed but not overwritten, so the old one moves aside as `<name>.old.exe`
/// until nothing runs it any more.
pub fn install(r: &Release) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bytes = get(&r.url)?;
    if (r.size > 0 && bytes.len() as u64 != r.size) || !bytes.starts_with(b"MZ") {
        return Err(format!("the download is broken ({} bytes)", bytes.len()));
    }
    let (new, old) = (beside(&exe, "new"), beside(&exe, "old"));
    std::fs::write(&new, &bytes).map_err(|e| format!("{}: {e}", new.display()))?;
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(&exe, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(format!("{}: {e}", exe.display()));
    }
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        let _ = std::fs::remove_file(&new);
        return Err(format!("{}: {e}", exe.display()));
    }
    config::log(&format!("installed {} as {}", r.version, exe.display()));
    Ok(exe)
}

/// After `install`: the old engine quits and the new exe takes over, as the editor (which starts
/// its own engine) when `editor`, otherwise as the engine if one was running. The caller closes
/// the old editor.
pub fn restart(exe: &Path, editor: bool) {
    let engine = config::engine_running();
    config::stop_engine(Duration::from_secs(5));
    if editor {
        config::spawn(exe, &["--updated"]);
    } else if engine {
        config::spawn(exe, &["--background"]);
    }
}

/// Removes what an update left behind. True when nothing is left; the old exe can only go once
/// nothing runs it.
pub fn tidy() -> bool {
    let Ok(exe) = std::env::current_exe() else { return true };
    ["old", "new"].iter().all(|tag| {
        let p = beside(&exe, tag);
        !p.exists() || std::fs::remove_file(&p).is_ok()
    })
}

fn beside(exe: &Path, tag: &str) -> PathBuf {
    let stem = exe.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "AbletonSkinLab".into());
    exe.with_file_name(format!("{stem}.{tag}.exe"))
}

struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// An HTTPS GET with Windows' own HTTP stack (proxy settings included), following redirects.
fn get(url: &str) -> Result<Vec<u8>, String> {
    let rest = url.strip_prefix("https://").ok_or("not an https link")?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let fail = |what: &str| format!("{what}: {}", windows::core::Error::from_win32());
    unsafe {
        let session = Handle(WinHttpOpen(w!("AbletonSkinLab-updater"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err(fail("WinHttpOpen"));
        }
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let connect = Handle(WinHttpConnect(session.0, &HSTRING::from(host), 443, 0));
        if connect.0.is_null() {
            return Err(fail(host));
        }
        let object = HSTRING::from(format!("/{path}"));
        let request = Handle(WinHttpOpenRequest(connect.0, w!("GET"), &object, PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE));
        if request.0.is_null() {
            return Err(fail("WinHttpOpenRequest"));
        }
        let headers: Vec<u16> = "Accept: application/vnd.github+json\r\n".encode_utf16().collect();
        let headers = if host == "api.github.com" { Some(headers.as_slice()) } else { None };
        WinHttpSendRequest(request.0, headers, None, 0, 0, 0).map_err(|e| format!("{host}: {e}"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| format!("{host}: {e}"))?;
        let (mut status, mut len) = (0u32, 4u32);
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut core::ffi::c_void),
            &mut len,
            std::ptr::null_mut(),
        )
        .map_err(|e| e.to_string())?;
        if status != 200 {
            return Err(format!("{host}: HTTP {status}"));
        }
        let mut out = Vec::new();
        loop {
            let mut avail = 0u32;
            WinHttpQueryDataAvailable(request.0, &mut avail).map_err(|e| e.to_string())?;
            if avail == 0 {
                return Ok(out);
            }
            let start = out.len();
            out.resize(start + avail as usize, 0);
            let mut read = 0u32;
            WinHttpReadData(request.0, out[start..].as_mut_ptr().cast(), avail, &mut read).map_err(|e| e.to_string())?;
            out.truncate(start + read as usize);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_compare_by_number() {
        assert!(super::newer("1.0.10", "1.0.9"));
        assert!(super::newer("1.1", "1.0.5"));
        assert!(!super::newer("1.0", "1.0.0"));
        assert!(!super::newer("0.9.9", "1.0.0"));
    }
}
