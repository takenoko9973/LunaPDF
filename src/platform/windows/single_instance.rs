use std::env;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result, anyhow};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, prelude::*};
use interprocess::os::windows::local_socket::ListenerOptionsExt;
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use widestring::{U16CStr, U16CString};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, TOKEN_GROUPS, TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TOKEN_USER,
    TokenRestrictedSids, TokenUser,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

const PROTOCOL_MAGIC: [u8; 4] = *b"LNP1";
// CreateProcessWのコマンドライン上限と同じ単位数を上限にし、壊れた要求による
// 過大な割り当てを防ぎつつ、Explorerから渡せる入力は欠落させない。
const MAX_REQUEST_UTF16_UNITS: usize = 32_767;
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

pub(crate) struct SingleInstanceListener {
    listener: LocalSocketListener,
}

impl SingleInstanceListener {
    /// 現在のユーザー用Named Pipeを所有するか、既存プロセスへ起動パスを転送する。
    ///
    /// `Some`は呼び出し側が主プロセスとして受信を開始する必要があることを表す。
    /// `None`は既存プロセスへの転送が完了し、このプロセスを終了できることを表す。
    pub(crate) fn acquire_or_forward(paths: &[PathBuf]) -> Result<Option<Self>> {
        let socket_name = socket_name_for_current_user()?;
        let listener_name = socket_name
            .as_str()
            .to_ns_name::<GenericNamespaced>()
            .context("encode LunaPDF single-instance listener name")?;
        // 保護されたDACLで所有ユーザーとSYSTEM以外のPipe接続を拒否する。
        // 受信後のSID照合も残し、名前の予測可能性を認証として使わない。
        let security_descriptor = current_user_pipe_security_descriptor()
            .context("create LunaPDF single-instance security descriptor")?;
        let listener_options = ListenerOptions::new()
            .name(listener_name)
            .security_descriptor(security_descriptor);
        match listener_options.create_sync() {
            Ok(listener) => Ok(Some(Self { listener })),
            Err(error) if existing_listener_error(&error) => {
                forward_paths(&socket_name, paths)
                    .context("forward PDF paths to the running LunaPDF instance")?;
                Ok(None)
            }
            Err(error) => Err(error).context("create LunaPDF single-instance listener"),
        }
    }

    /// 受信ループを専用スレッドで開始し、各接続の結果をUI側コールバックへ渡す。
    ///
    /// 接続ごとに読み取りスレッドを分けるため、起動途中で停止した送信元があっても、
    /// 後続のExplorer要求を受け付ける受信ループはブロックされない。
    pub(crate) fn spawn<F>(self, on_event: F) -> io::Result<()>
    where
        F: Fn(std::result::Result<Vec<PathBuf>, String>) + Send + Sync + 'static,
    {
        let on_event = Arc::new(on_event);
        thread::Builder::new()
            .name("lunapdf-instance-listener".to_owned())
            .spawn(move || {
                loop {
                    let connection = match self.listener.accept() {
                        Ok(connection) => connection,
                        Err(error) => {
                            on_event(Err(format!(
                                "既存ウィンドウへの起動要求を受信できませんでした: {error}"
                            )));
                            break;
                        }
                    };
                    let request_callback = Arc::clone(&on_event);
                    let request_thread = thread::Builder::new()
                        .name("lunapdf-instance-request".to_owned())
                        .spawn(move || {
                            let mut connection = connection;
                            let event = verify_same_user_peer(&connection)
                                .and_then(|()| read_paths_from(&mut connection))
                                .map_err(|error| {
                                    format!("受信した起動要求を読み取れませんでした: {error}")
                                });
                            request_callback(event);
                        });
                    if let Err(error) = request_thread {
                        on_event(Err(format!(
                            "起動要求の受信スレッドを開始できませんでした: {error}"
                        )));
                    }
                }
            })
            .map(|_handle| ())
    }
}

fn existing_listener_error(error: &io::Error) -> bool {
    // Windows Named Pipeでは、同名のサーバー端が既に作られている場合に
    // AddrInUseではなくPermissionDeniedが返る実装がある。どちらも接続を試し、
    // 実際に既存プロセスへ書けなければforward_paths側のエラーを返す。
    matches!(
        error.kind(),
        io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied
    )
}

fn forward_paths(socket_name: &str, paths: &[PathBuf]) -> Result<()> {
    let name = socket_name
        .to_ns_name::<GenericNamespaced>()
        .context("encode LunaPDF single-instance client name")?;
    let mut connection =
        LocalSocketStream::connect(name).context("connect to the running LunaPDF instance")?;
    verify_same_user_peer(&connection).context("authenticate the running LunaPDF instance user")?;
    write_paths_to(&mut connection, paths).context("write LunaPDF open request")?;
    connection.flush().context("flush LunaPDF open request")?;
    Ok(())
}

fn verify_same_user_peer(connection: &LocalSocketStream) -> io::Result<()> {
    let peer_pid = connection
        .peer_creds()?
        .pid()
        .ok_or_else(|| invalid_request("the local socket did not report a peer process"))?;
    if processes_share_user(std::process::id(), peer_pid)? {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "the local socket peer belongs to a different Windows user",
    ))
}

fn processes_share_user(first_pid: u32, second_pid: u32) -> io::Result<bool> {
    let first_token_user = token_user_for_process(first_pid)?;
    let second_token_user = token_user_for_process(second_pid)?;
    let first_sid = token_user_sid(&first_token_user);
    let second_sid = token_user_sid(&second_token_user);
    Ok(sids_equal(first_sid, second_sid))
}

fn sids_equal(first_sid: *mut core::ffi::c_void, second_sid: *mut core::ffi::c_void) -> bool {
    // SAFETY: 呼び出し側は両ポインタの元バッファをEqualSid完了まで保持する。
    unsafe { EqualSid(first_sid, second_sid) != 0 }
}

fn current_user_pipe_security_descriptor() -> Result<SecurityDescriptor> {
    let token_user = token_user_for_process(std::process::id())?;
    let user_sid = sid_text(token_user_sid(&token_user))?;
    let restricted_sids = restricted_sid_texts_for_process(std::process::id())?;
    let mut access_entries = format!("(A;;GA;;;{user_sid})");
    for restricted_sid in restricted_sids {
        // Windowsの制限トークンは、通常SIDと制限SIDの両方でアクセス検査を
        // 通過する必要があるため、現プロセスに実在する制限SIDも明示する。
        access_entries.push_str(&format!("(A;;GA;;;{restricted_sid})"));
    }
    access_entries.push_str("(A;;GA;;;SY)");

    // 実際のユーザーSIDと現トークンの制限SIDだけをDACLへ固定する。
    let descriptor_text = format!("D:P{access_entries}");
    let descriptor_text = U16CString::from_str(descriptor_text)
        .context("encode LunaPDF single-instance security descriptor")?;
    SecurityDescriptor::deserialize(&descriptor_text)
        .context("deserialize LunaPDF single-instance security descriptor")
}

fn sid_text(sid: *mut core::ffi::c_void) -> io::Result<String> {
    let mut string_sid = std::ptr::null_mut();
    // SAFETY: sidは有効なTOKEN_USERバッファを指し、string_sidはAPIが初期化できる。
    if unsafe { ConvertSidToStringSidW(sid, &mut string_sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ConvertSidToStringSidWはNULL終端のLocalAlloc領域を返す。
    // 文字列へ複製した直後にLocalFreeし、ポインタを保持しない。
    let text = unsafe { U16CStr::from_ptr_str(string_sid) }.to_string_lossy();
    // SAFETY: string_sidは上のAPIがLocalAllocで割り当てた領域で、解放は一度だけ。
    unsafe {
        LocalFree(string_sid.cast());
    }
    Ok(text)
}

fn token_user_for_process(process_id: u32) -> io::Result<Vec<usize>> {
    token_information_for_process(process_id, TokenUser)
}

fn restricted_sid_texts_for_process(process_id: u32) -> io::Result<Vec<String>> {
    let buffer = token_information_for_process(process_id, TokenRestrictedSids)?;
    // SAFETY: token_information_for_processがTokenRestrictedSidsで初期化したバッファの
    // 先頭にTOKEN_GROUPSがあり、usize配列により必要なアライメントも保つ。
    let groups = unsafe { &*buffer.as_ptr().cast::<TOKEN_GROUPS>() };
    let group_count = groups.GroupCount as usize;
    let group_bytes = size_of::<windows_sys::Win32::Security::SID_AND_ATTRIBUTES>()
        .checked_mul(group_count)
        .ok_or_else(|| invalid_request("Windows restricted SID count is too large"))?;
    let required_bytes = std::mem::offset_of!(TOKEN_GROUPS, Groups)
        .checked_add(group_bytes)
        .ok_or_else(|| invalid_request("Windows restricted SID buffer size overflow"))?;
    if required_bytes > std::mem::size_of_val(buffer.as_slice()) {
        return Err(invalid_request(
            "Windows restricted SID buffer is truncated",
        ));
    }
    // SAFETY: 上でGroupCountに対応する領域がバッファ内に収まることを確認した。
    let groups = unsafe { std::slice::from_raw_parts(groups.Groups.as_ptr(), group_count) };
    groups.iter().map(|group| sid_text(group.Sid)).collect()
}

fn token_information_for_process(
    process_id: u32,
    information_class: TOKEN_INFORMATION_CLASS,
) -> io::Result<Vec<usize>> {
    // SAFETY: PIDはOSから取得した値で、継承しない照会専用ハンドルだけを要求する。
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    let process = OwnedHandle::new(process)?;
    let mut token = std::ptr::null_mut();
    // SAFETY: processは有効なプロセスハンドルで、tokenはAPIが書き込める領域を指す。
    if unsafe { OpenProcessToken(process.get(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle::new(token)?;

    let mut required_bytes = 0u32;
    // SAFETY: 最初の呼び出しは必要サイズの照会で、NULLバッファと長さ0を指定する。
    unsafe {
        GetTokenInformation(
            token.get(),
            information_class,
            std::ptr::null_mut(),
            0,
            &mut required_bytes,
        );
    }
    if required_bytes == 0 {
        return Err(io::Error::last_os_error());
    }
    let word_size = size_of::<usize>();
    let word_count = (required_bytes as usize).div_ceil(word_size);
    let mut buffer = vec![0usize; word_count];
    let buffer_bytes = u32::try_from(buffer.len() * word_size)
        .map_err(|_| invalid_request("Windows token information is too large"))?;
    // SAFETY: bufferはrequired_bytes以上でusize境界に整列し、APIの出力先として有効。
    if unsafe {
        GetTokenInformation(
            token.get(),
            information_class,
            buffer.as_mut_ptr().cast(),
            buffer_bytes,
            &mut required_bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

fn token_user_sid(buffer: &[usize]) -> *mut core::ffi::c_void {
    // SAFETY: token_user_for_processだけがこの関数へバッファを渡し、先頭には
    // GetTokenInformation(TokenUser)が初期化したTOKEN_USERが格納されている。
    unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid }
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE) -> io::Result<Self> {
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }

    fn get(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: OwnedHandleは有効な所有ハンドルだけを保持し、Dropは一度だけ呼ばれる。
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn socket_name_for_current_user() -> Result<String> {
    let appdata = env::var_os("APPDATA")
        .ok_or_else(|| anyhow!("APPDATA is not set; cannot scope the LunaPDF instance"))?;
    // PipeのDACLと双方向のSID照合がアクセスを制限する。APPDATAの安定ハッシュは
    // 異なるユーザー同士が同じPipe名を奪い合わないための名前空間分離だけを担う。
    let mut user_hash = FNV_OFFSET_BASIS;
    for unit in appdata.encode_wide() {
        for byte in unit.to_le_bytes() {
            user_hash ^= u64::from(byte);
            user_hash = user_hash.wrapping_mul(FNV_PRIME);
        }
    }
    Ok(format!("lunapdf-{user_hash:016x}-v1"))
}

fn write_paths_to(writer: &mut impl Write, paths: &[PathBuf]) -> io::Result<()> {
    let encoded_paths = encode_paths(paths)?;
    writer.write_all(&PROTOCOL_MAGIC)?;
    write_u32(writer, encoded_paths.len())?;
    for path in encoded_paths {
        write_u32(writer, path.len())?;
        for unit in path {
            writer.write_all(&unit.to_le_bytes())?;
        }
    }
    Ok(())
}

fn encode_paths(paths: &[PathBuf]) -> io::Result<Vec<Vec<u16>>> {
    if paths.len() > MAX_REQUEST_UTF16_UNITS {
        return Err(invalid_request("too many paths in one request"));
    }
    let mut total_units = 0usize;
    let mut encoded_paths = Vec::with_capacity(paths.len());
    for path in paths {
        let encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
        total_units = total_units
            .checked_add(encoded.len())
            .ok_or_else(|| invalid_request("path length overflow"))?;
        if total_units > MAX_REQUEST_UTF16_UNITS {
            return Err(invalid_request(
                "request exceeds the Windows command-line limit",
            ));
        }
        encoded_paths.push(encoded);
    }
    Ok(encoded_paths)
}

fn read_paths_from(reader: &mut impl Read) -> io::Result<Vec<PathBuf>> {
    let mut magic = [0u8; PROTOCOL_MAGIC.len()];
    reader.read_exact(&mut magic)?;
    if magic != PROTOCOL_MAGIC {
        return Err(invalid_request("unsupported single-instance protocol"));
    }

    let path_count = read_u32(reader)? as usize;
    if path_count > MAX_REQUEST_UTF16_UNITS {
        return Err(invalid_request("too many paths in one request"));
    }
    let mut total_units = 0usize;
    let mut paths = Vec::with_capacity(path_count);
    for _ in 0..path_count {
        let unit_count = read_u32(reader)? as usize;
        total_units = total_units
            .checked_add(unit_count)
            .ok_or_else(|| invalid_request("path length overflow"))?;
        if total_units > MAX_REQUEST_UTF16_UNITS {
            return Err(invalid_request(
                "request exceeds the Windows command-line limit",
            ));
        }
        let mut path_bytes = vec![0u8; unit_count * size_of::<u16>()];
        reader.read_exact(&mut path_bytes)?;
        let units = path_bytes
            .chunks_exact(size_of::<u16>())
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();
        paths.push(PathBuf::from(OsString::from_wide(&units)));
    }
    Ok(paths)
}

fn write_u32(writer: &mut impl Write, value: usize) -> io::Result<()> {
    let value = u32::try_from(value).map_err(|_| invalid_request("request field is too large"))?;
    writer.write_all(&value.to_le_bytes())
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0u8; size_of::<u32>()];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn invalid_request(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::io::Cursor;

    use super::*;
    use windows_sys::Win32::Security::{
        CreateWellKnownSid, SECURITY_MAX_SID_SIZE, WinLocalSystemSid,
    };

    #[test]
    fn protocol_roundtrip_preserves_multiple_windows_paths() {
        let paths = vec![
            PathBuf::from(r"C:\PDF files\first paper.pdf"),
            PathBuf::from(r"C:\資料\月面観測.pdf"),
        ];
        let mut bytes = Vec::new();

        write_paths_to(&mut bytes, &paths).unwrap();

        assert_eq!(read_paths_from(&mut Cursor::new(bytes)).unwrap(), paths);
    }

    #[test]
    fn protocol_allows_an_empty_request_to_focus_the_existing_window() {
        let mut bytes = Vec::new();

        write_paths_to(&mut bytes, &[]).unwrap();

        assert!(read_paths_from(&mut Cursor::new(bytes)).unwrap().is_empty());
    }

    #[test]
    fn protocol_rejects_unknown_versions() {
        let mut bytes = Vec::from(*b"LNP2");
        bytes.extend_from_slice(&0u32.to_le_bytes());

        let error = read_paths_from(&mut Cursor::new(bytes)).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn protocol_rejects_requests_larger_than_create_process_accepts() {
        let path = PathBuf::from(OsStr::new(&"x".repeat(MAX_REQUEST_UTF16_UNITS + 1)));

        let error = write_paths_to(&mut Vec::new(), &[path]).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn occupied_pipe_errors_both_select_the_client_path() {
        let address_in_use = io::Error::from(io::ErrorKind::AddrInUse);
        let permission_denied = io::Error::from(io::ErrorKind::PermissionDenied);
        let unrelated = io::Error::from(io::ErrorKind::NotFound);

        assert!(existing_listener_error(&address_in_use));
        assert!(existing_listener_error(&permission_denied));
        assert!(!existing_listener_error(&unrelated));
    }

    #[test]
    fn current_process_token_matches_its_own_windows_user() {
        let process_id = std::process::id();

        assert!(processes_share_user(process_id, process_id).unwrap());
    }

    #[test]
    fn current_user_sid_is_rejected_when_compared_with_local_system() {
        let current_user = token_user_for_process(std::process::id()).unwrap();
        let current_user_sid = token_user_sid(&current_user);
        let word_count = (SECURITY_MAX_SID_SIZE as usize).div_ceil(size_of::<usize>());
        let mut system_sid = vec![0usize; word_count];
        let mut system_sid_bytes =
            u32::try_from(std::mem::size_of_val(system_sid.as_slice())).unwrap();
        // SAFETY: system_sidは要求される最大SIDサイズ以上でusize境界に整列している。
        let created = unsafe {
            CreateWellKnownSid(
                WinLocalSystemSid,
                std::ptr::null_mut(),
                system_sid.as_mut_ptr().cast(),
                &mut system_sid_bytes,
            )
        };
        assert_ne!(created, 0);

        assert!(!sids_equal(
            current_user_sid,
            system_sid.as_mut_ptr().cast()
        ));
    }

    #[test]
    fn protected_listener_forwards_paths_for_its_own_windows_user() {
        let socket_name = format!(
            "lunapdf-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let listener_name = socket_name
            .as_str()
            .to_ns_name::<GenericNamespaced>()
            .unwrap();
        let listener = ListenerOptions::new()
            .name(listener_name)
            .security_descriptor(current_user_pipe_security_descriptor().unwrap())
            .create_sync()
            .unwrap();
        let listener = SingleInstanceListener { listener };
        let (sender, receiver) = crossbeam_channel::bounded(1);
        listener
            .spawn(move |event| sender.send(event).unwrap())
            .unwrap();
        let paths = vec![
            PathBuf::from(r"C:\PDF files\first paper.pdf"),
            PathBuf::from(r"C:\資料\月面観測.pdf"),
        ];

        forward_paths(&socket_name, &paths).unwrap();
        let forwarded = receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();

        assert_eq!(forwarded.unwrap(), paths);
    }
}
