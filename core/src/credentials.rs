use serde::Serialize;

use crate::providers::{self, ALIBABA_PROVIDER, OPENAI_PROVIDER};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CredentialStatus {
    pub configured: bool,
    pub stored_configured: bool,
    pub environment_override: bool,
    pub source: Option<&'static str>,
}

const EXTERNAL_API_TARGET: &str = "VRCS/ExternalAPI/token";
const EXTERNAL_API_ENV: &str = "VRCS_EXTERNAL_API_TOKEN";
const VRCX_TARGET: &str = "VRCS/VRCXIntegration/token";
const VRCX_ENV: &str = "VRCS_VRCX_INTEGRATION_TOKEN";

pub fn external_api_token_status() -> Result<CredentialStatus, String> {
    let environment_override =
        std::env::var(EXTERNAL_API_ENV).is_ok_and(|value| !value.trim().is_empty());
    let stored_configured = read_stored(EXTERNAL_API_TARGET)?.is_some();
    Ok(CredentialStatus {
        configured: environment_override || stored_configured,
        stored_configured,
        environment_override,
        source: if environment_override {
            Some("environment")
        } else {
            stored_configured.then_some("credential_manager")
        },
    })
}

pub fn read_external_api_token() -> Result<Option<String>, String> {
    if let Ok(value) = std::env::var(EXTERNAL_API_ENV) {
        if !value.trim().is_empty() {
            return Ok(Some(value));
        }
    }
    read_stored_external_api_token()
}

pub(crate) fn read_stored_external_api_token() -> Result<Option<String>, String> {
    read_stored(EXTERNAL_API_TARGET)
}

pub fn write_external_api_token(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 4096 {
        return Err("External API token length is invalid".into());
    }
    write_stored(EXTERNAL_API_TARGET, value)
}

pub fn delete_external_api_token() -> Result<(), String> {
    delete_stored(EXTERNAL_API_TARGET)
}

pub fn vrcx_token_status() -> Result<CredentialStatus, String> {
    let environment_override = std::env::var(VRCX_ENV).is_ok_and(|value| !value.trim().is_empty());
    let stored_configured = read_stored(VRCX_TARGET)?.is_some();
    Ok(CredentialStatus {
        configured: environment_override || stored_configured,
        stored_configured,
        environment_override,
        source: if environment_override {
            Some("environment")
        } else {
            stored_configured.then_some("credential_manager")
        },
    })
}

pub fn read_vrcx_token() -> Result<Option<String>, String> {
    if let Ok(value) = std::env::var(VRCX_ENV) {
        if !value.trim().is_empty() {
            return Ok(Some(value));
        }
    }
    read_stored_vrcx_token()
}

pub(crate) fn read_stored_vrcx_token() -> Result<Option<String>, String> {
    read_stored(VRCX_TARGET)
}

pub fn write_vrcx_token(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 4096 {
        return Err("VRCX-0 token length is invalid".into());
    }
    write_stored(VRCX_TARGET, value)
}

pub fn delete_vrcx_token() -> Result<(), String> {
    delete_stored(VRCX_TARGET)
}

fn environment_variables(
    provider: &str,
) -> Result<(&'static [&'static str], &'static [&'static str]), String> {
    let definition = providers::definition(provider)
        .ok_or_else(|| format!("Unsupported API provider: {provider}"))?;
    Ok((
        definition.connection.environment_variables,
        definition.connection.legacy_environment_variables,
    ))
}

fn validate_provider(provider: &str) -> Result<(), String> {
    environment_variables(provider).map(|_| ())
}

pub fn credential_status(profile_id: &str, provider: &str) -> Result<CredentialStatus, String> {
    validate_provider(provider)?;
    let (primary, legacy) = environment_variables(provider)?;
    let environment_override = primary
        .iter()
        .chain(legacy)
        .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()));
    let stored_configured = read_profile_stored(profile_id, provider)?.is_some();
    Ok(CredentialStatus {
        configured: environment_override || stored_configured,
        stored_configured,
        environment_override,
        source: if environment_override {
            Some("environment")
        } else {
            stored_configured.then_some("credential_manager")
        },
    })
}

pub fn read_credential(profile_id: &str, provider: &str) -> Result<Option<String>, String> {
    let (primary, legacy) = environment_variables(provider)?;
    for name in primary.iter().chain(legacy) {
        if let Ok(value) = std::env::var(name) {
            if !value.trim().is_empty() {
                return Ok(Some(value));
            }
        }
    }
    read_stored_credential(profile_id, provider)
}

pub(crate) fn read_stored_credential(
    profile_id: &str,
    provider: &str,
) -> Result<Option<String>, String> {
    validate_provider(provider)?;
    read_profile_stored(profile_id, provider)
}

pub fn write_credential(profile_id: &str, provider: &str, value: &str) -> Result<(), String> {
    validate_provider(provider)?;
    let value = value.trim();
    if value.is_empty() || value.len() > 4096 {
        return Err("API key length is invalid".into());
    }
    write_stored(&target(profile_id), value)
}

pub fn delete_credential(profile_id: &str, provider: &str) -> Result<(), String> {
    validate_provider(provider)?;
    delete_stored(&target(profile_id))?;
    delete_stored(&old_profile_target(profile_id))?;
    if let Some(target) = legacy_target(profile_id, provider) {
        delete_stored(&target)?;
    }
    Ok(())
}

fn target(profile_id: &str) -> String {
    format!("VRCS/API/profile/{profile_id}")
}

fn old_profile_target(profile_id: &str) -> String {
    format!("VRCS/ASR/profile/{profile_id}")
}

fn legacy_target(profile_id: &str, provider: &str) -> Option<String> {
    match (profile_id, provider) {
        ("legacy-alibaba-cloud", ALIBABA_PROVIDER) => Some("VRCS/ASR/qwen".into()),
        ("legacy-openai", OPENAI_PROVIDER) => Some("VRCS/ASR/openai".into()),
        _ => None,
    }
}

fn read_profile_stored(profile_id: &str, provider: &str) -> Result<Option<String>, String> {
    for target in [target(profile_id), old_profile_target(profile_id)] {
        if let Some(value) = read_stored(&target)? {
            return Ok(Some(value));
        }
    }
    match legacy_target(profile_id, provider) {
        Some(target) => read_stored(&target),
        None => Ok(None),
    }
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn read_stored(target_name: &str) -> Result<Option<String>, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Security::Credentials::{
        CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
    };

    let target = wide(target_name);
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    if let Err(error) = unsafe {
        CredReadW(
            PCWSTR(target.as_ptr()),
            CRED_TYPE_GENERIC,
            None,
            &mut credential,
        )
    } {
        if error.code().0 == 0x80070490u32 as i32 {
            return Ok(None);
        }
        return Err(format!("Failed to read Windows credential: {error}"));
    }
    if credential.is_null() {
        return Ok(None);
    }
    let result = unsafe {
        let credential = &*credential;
        let bytes = std::slice::from_raw_parts(
            credential.CredentialBlob,
            credential.CredentialBlobSize as usize,
        );
        String::from_utf8(bytes.to_vec())
            .map_err(|_| "Windows credential is not valid UTF-8".to_string())
    };
    unsafe { CredFree(credential.cast()) };
    result.map(Some)
}

#[cfg(windows)]
fn write_stored(target_name: &str, value: &str) -> Result<(), String> {
    use windows::core::PWSTR;
    use windows::Win32::Security::Credentials::{
        CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    };

    let mut target = wide(target_name);
    let mut username = wide("VRCS");
    let mut blob = value.as_bytes().to_vec();
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(username.as_mut_ptr()),
        ..Default::default()
    };
    unsafe { CredWriteW(&credential, 0) }
        .map_err(|error| format!("Failed to write Windows credential: {error}"))
}

#[cfg(windows)]
fn delete_stored(target_name: &str) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};

    let target = wide(target_name);
    match unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) } {
        Ok(()) => Ok(()),
        Err(error) if error.code().0 == 0x80070490u32 as i32 => Ok(()),
        Err(error) => Err(format!("Failed to delete Windows credential: {error}")),
    }
}

#[cfg(not(windows))]
use std::collections::BTreeMap;
#[cfg(not(windows))]
use std::io::Write;
#[cfg(not(windows))]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(not(windows))]
use std::path::{Path, PathBuf};

/// 凭据文件在数据目录内的相对位置。
#[cfg(not(windows))]
const CREDENTIALS_FILE: &str = "vrcs/credentials.json";

/// 解析数据目录：`$XDG_DATA_HOME` 只在绝对路径时生效（相对路径按 XDG 规范忽略），
/// 否则回退到 `$HOME/.local/share`。
#[cfg(not(windows))]
fn data_home_from(
    xdg_data_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if let Some(path) = xdg_data_home.filter(|path| path.is_absolute()) {
        return Ok(path);
    }
    let home = home.ok_or_else(|| {
        "Cannot locate the credential store: neither XDG_DATA_HOME nor HOME is set".to_string()
    })?;
    Ok(home.join(".local/share"))
}

#[cfg(not(windows))]
fn data_home() -> Result<PathBuf, String> {
    data_home_from(
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// 凭据文件路径；参数化数据目录是为了让测试不必改动进程级环境变量。
#[cfg(not(windows))]
fn store_path_in(data_home: &Path) -> PathBuf {
    data_home.join(CREDENTIALS_FILE)
}

#[cfg(not(windows))]
fn store_path() -> Result<PathBuf, String> {
    Ok(store_path_in(&data_home()?))
}

/// 读取整份凭据文件。文件不存在或内容为空视为空存储；内容损坏时如实报错，
/// 而不是静默当成空存储、把别的密钥覆盖掉。权限过宽不报错，读取路径也不放宽任何权限。
#[cfg(not(windows))]
fn read_store(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(format!(
                "Failed to read credential store {}: {error}",
                path.display()
            ))
        }
    };
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_str(&text).map_err(|error| {
        format!(
            "Failed to parse credential store {}: {error}",
            path.display()
        )
    })
}

/// 同目录临时文件 → 0600 → `rename` 原子替换：读者要么看到旧文件、要么看到新文件，
/// 不会读到写了一半的 JSON。
#[cfg(not(windows))]
fn write_store(path: &Path, credentials: &BTreeMap<String, String>) -> Result<(), String> {
    let directory = path
        .parent()
        .ok_or_else(|| format!("Invalid credential store path: {}", path.display()))?;
    std::fs::create_dir_all(directory).map_err(|error| {
        format!(
            "Failed to create credential directory {}: {error}",
            directory.display()
        )
    })?;
    let json = serde_json::to_string(credentials)
        .map_err(|error| format!("Failed to serialize credentials: {error}"))?;
    let temporary = temporary_path(path);
    if let Err(error) = write_private_file(&temporary, json.as_bytes()) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!(
            "Failed to replace credential store {}: {error}",
            path.display()
        ));
    }
    Ok(())
}

/// 用 `create_new` 新建（不会跟随已存在的符号链接），显式设置权限后再写入。
#[cfg(not(windows))]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| {
            format!(
                "Failed to create temporary credential file {}: {error}",
                path.display()
            )
        })?;
    // 创建权限会被 umask 削减，这里显式收紧，保证 group/other 读不到。
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("Failed to restrict credential permissions: {error}"))?;
    file.write_all(bytes).map_err(|error| {
        format!(
            "Failed to write credential store {}: {error}",
            path.display()
        )
    })?;
    file.sync_all().map_err(|error| {
        format!(
            "Failed to flush credential store {}: {error}",
            path.display()
        )
    })
}

/// 临时文件必须落在凭据文件同目录（`rename` 不能跨文件系统），名字带上 pid/时间戳/序号避免并发撞名。
#[cfg(not(windows))]
fn temporary_path(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!(
        ".credentials-{}-{nanos:x}-{sequence}.tmp",
        std::process::id()
    ))
}

#[cfg(not(windows))]
fn read_value(path: &Path, target_name: &str) -> Result<Option<String>, String> {
    let mut credentials = read_store(path)?;
    Ok(credentials.remove(target_name))
}

/// 在凭据文件旁的锁文件上持有独占的建议锁，执行一次读-改-写。
///
/// 原子 `rename` 只保证读者看不到写了一半的文件；两个写者（例如桌面壳内置的 Core
/// 与单独启动的开发用 Core）若各自读到旧内容再写回，后写的会把先写的键覆盖掉。
/// 锁加在单独的文件上，因为凭据文件本身每次都会被 `rename` 替换成新的 inode。
/// 锁随文件句柄关闭（包括进程退出）自动释放。读路径不加锁。
#[cfg(not(windows))]
fn with_store_lock<T>(
    path: &Path,
    update: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let directory = path
        .parent()
        .ok_or_else(|| format!("Invalid credential store path: {}", path.display()))?;
    std::fs::create_dir_all(directory).map_err(|error| {
        format!(
            "Failed to create credential directory {}: {error}",
            directory.display()
        )
    })?;
    let lock_path = path.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|error| {
            format!(
                "Failed to open credential lock {}: {error}",
                lock_path.display()
            )
        })?;
    lock.lock().map_err(|error| {
        format!(
            "Failed to lock credential store {}: {error}",
            lock_path.display()
        )
    })?;
    update()
}

#[cfg(not(windows))]
fn write_value(path: &Path, target_name: &str, value: &str) -> Result<(), String> {
    with_store_lock(path, || {
        let mut credentials = read_store(path)?;
        credentials.insert(target_name.to_string(), value.to_string());
        write_store(path, &credentials)
    })
}

/// 条目或文件不存在同样算删除成功，与 Windows 分支语义一致。
#[cfg(not(windows))]
fn delete_value(path: &Path, target_name: &str) -> Result<(), String> {
    with_store_lock(path, || {
        let mut credentials = read_store(path)?;
        if credentials.remove(target_name).is_none() {
            return Ok(());
        }
        write_store(path, &credentials)
    })
}

#[cfg(not(windows))]
fn read_stored(target_name: &str) -> Result<Option<String>, String> {
    read_value(&store_path()?, target_name)
}

#[cfg(not(windows))]
fn write_stored(target_name: &str, value: &str) -> Result<(), String> {
    write_value(&store_path()?, target_name, value)
}

#[cfg(not(windows))]
fn delete_stored(target_name: &str) -> Result<(), String> {
    delete_value(&store_path()?, target_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_all_supported_providers() {
        for provider in providers::catalog() {
            assert!(credential_status("profile", provider.id).is_ok());
        }
        assert!(credential_status("profile", "unknown").is_err());
        assert!(write_credential("profile", ALIBABA_PROVIDER, " ").is_err());
    }

    #[test]
    fn brand_environment_variables_keep_the_compatible_fallback() {
        let (primary, legacy) = environment_variables(providers::GROQ_PROVIDER).unwrap();
        assert_eq!(primary, ["VRCS_GROQ_API_KEY", "GROQ_API_KEY"]);
        assert_eq!(legacy, ["VRCS_OPENAI_COMPATIBLE_API_KEY"]);
    }

    #[test]
    fn legacy_profiles_keep_the_previous_targets() {
        assert_eq!(
            legacy_target("legacy-alibaba-cloud", ALIBABA_PROVIDER).as_deref(),
            Some("VRCS/ASR/qwen")
        );
        assert!(legacy_target("new-profile", ALIBABA_PROVIDER).is_none());
    }

    #[test]
    fn integration_tokens_use_independent_credential_targets() {
        assert_eq!(EXTERNAL_API_TARGET, "VRCS/ExternalAPI/token");
        assert_eq!(VRCX_TARGET, "VRCS/VRCXIntegration/token");
        assert_ne!(EXTERNAL_API_TARGET, VRCX_TARGET);
        assert_ne!(VRCX_TARGET, target("profile"));
        assert!(write_external_api_token(" ").is_err());
        assert!(write_vrcx_token(" ").is_err());
    }

    #[cfg(not(windows))]
    use std::path::{Path, PathBuf};

    /// 隔离的凭据文件路径：数据目录刻意不存在，用来验证写入时会自行创建目录层级。
    #[cfg(not(windows))]
    fn temporary_store() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = store_path_in(&directory.path().join("data"));
        (directory, path)
    }

    #[cfg(not(windows))]
    #[test]
    fn credential_store_path_follows_the_xdg_layout() {
        assert_eq!(
            store_path_in(Path::new("/home/dev/.local/share")),
            PathBuf::from("/home/dev/.local/share/vrcs/credentials.json")
        );
        assert_eq!(
            store_path_in(Path::new("/data")),
            PathBuf::from("/data/vrcs/credentials.json")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn data_home_prefers_xdg_and_falls_back_to_the_home_directory() {
        let home = PathBuf::from("/home/dev");
        assert_eq!(
            data_home_from(Some(PathBuf::from("/data")), Some(home.clone())).unwrap(),
            PathBuf::from("/data")
        );
        assert_eq!(
            data_home_from(None, Some(home.clone())).unwrap(),
            PathBuf::from("/home/dev/.local/share")
        );
        // XDG 规范：相对路径无效，退回 HOME。
        assert_eq!(
            data_home_from(Some(PathBuf::from("relative")), Some(home)).unwrap(),
            PathBuf::from("/home/dev/.local/share")
        );
        assert!(data_home_from(None, None).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn stored_credentials_round_trip_through_the_store_file() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, path) = temporary_store();
        assert_eq!(read_value(&path, "VRCS/ExternalAPI/token").unwrap(), None);

        write_value(&path, "VRCS/ExternalAPI/token", "secret").unwrap();

        assert_eq!(
            read_value(&path, "VRCS/ExternalAPI/token")
                .unwrap()
                .as_deref(),
            Some("secret")
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let stored: std::collections::HashMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            stored.get("VRCS/ExternalAPI/token").map(String::as_str),
            Some("secret")
        );
        // 原子替换后不应留下临时文件；锁文件是常驻的，权限同样收紧。
        let mut entries: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(entries, ["credentials.json", "credentials.lock"]);
        let lock_mode = std::fs::metadata(path.with_extension("lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(lock_mode & 0o077, 0);
    }

    #[cfg(not(windows))]
    #[test]
    fn stored_credentials_are_independent_per_target() {
        let (_directory, path) = temporary_store();
        write_value(&path, "VRCS/ExternalAPI/token", "first").unwrap();
        write_value(&path, "VRCS/VRCXIntegration/token", "second").unwrap();

        write_value(&path, "VRCS/ExternalAPI/token", "updated").unwrap();

        assert_eq!(
            read_value(&path, "VRCS/ExternalAPI/token")
                .unwrap()
                .as_deref(),
            Some("updated")
        );
        assert_eq!(
            read_value(&path, "VRCS/VRCXIntegration/token")
                .unwrap()
                .as_deref(),
            Some("second")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn deleting_stored_credentials_is_idempotent() {
        let (_directory, path) = temporary_store();
        // 文件还不存在时删除也要成功。
        assert!(delete_value(&path, "VRCS/ExternalAPI/token").is_ok());

        write_value(&path, "VRCS/ExternalAPI/token", "secret").unwrap();
        assert!(delete_value(&path, "VRCS/ExternalAPI/token").is_ok());
        assert_eq!(read_value(&path, "VRCS/ExternalAPI/token").unwrap(), None);
        // 条目已不存在时再次删除仍是成功。
        assert!(delete_value(&path, "VRCS/ExternalAPI/token").is_ok());
    }

    /// 读-改-写之间没有互斥时，两个写者会各自读到旧内容，后写的把先写的键覆盖掉。
    /// 每个线程各自打开文件，与两个进程共用同一份凭据文件时的情形相同。
    #[cfg(not(windows))]
    #[test]
    fn concurrent_writers_do_not_lose_each_others_keys() {
        const WRITERS: usize = 8;
        const KEYS_PER_WRITER: usize = 25;

        let (_directory, path) = temporary_store();
        let threads: Vec<_> = (0..WRITERS)
            .map(|writer| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for key in 0..KEYS_PER_WRITER {
                        write_value(&path, &format!("VRCS/test/{writer}/{key}"), "secret").unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }

        let stored = read_store(&path).unwrap();
        assert_eq!(stored.len(), WRITERS * KEYS_PER_WRITER);
    }

    #[cfg(not(windows))]
    #[test]
    fn a_corrupt_store_file_is_reported_instead_of_overwritten() {
        let (_directory, path) = temporary_store();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();

        assert!(read_value(&path, "VRCS/ExternalAPI/token").is_err());
        assert!(write_value(&path, "VRCS/ExternalAPI/token", "secret").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
    }
}
