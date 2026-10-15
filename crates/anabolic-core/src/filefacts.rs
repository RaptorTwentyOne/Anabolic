//! What a program file is: who signed it, its SHA-256, how it was built.
//! Reads the file only; nothing runs. The signature check works offline
//! (no revocation lookups) and accepts catalog signatures, which is how
//! most of Windows' own files are signed.

use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Security::Cryptography::Catalog::{
    CATALOG_INFO, CryptCATAdminAcquireContext2, CryptCATAdminCalcHashFromFileHandle2,
    CryptCATAdminEnumCatalogFromHash, CryptCATAdminReleaseCatalogContext,
    CryptCATAdminReleaseContext, CryptCATCatalogInfoFromContext,
};
use windows::Win32::Security::Cryptography::{
    BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS,
    BCRYPT_SHA256_ALGORITHM, BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash,
    BCryptFinishHash, BCryptHashData, BCryptOpenAlgorithmProvider, CERT_NAME_SIMPLE_DISPLAY_TYPE,
    CertGetNameStringW,
};
use windows::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_CATALOG_INFO, WINTRUST_DATA, WINTRUST_DATA_0,
    WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_CATALOG, WTD_CHOICE_FILE,
    WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
    WTD_UI_NONE, WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
};
use windows::core::{GUID, PCWSTR};

use crate::util::wide;

#[derive(Clone, Debug, PartialEq)]
pub enum Signature {
    /// Valid; `catalog` when signed through a Windows catalog rather than
    /// inside the file.
    Signed {
        signer: Option<String>,
        catalog: bool,
    },
    Unsigned,
    /// Signed, but the signature does not hold (tampered, untrusted root,
    /// expired without a timestamp…): Windows' reason.
    Invalid(String),
}

/// What the PE header says about how the file was built.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeFacts {
    pub machine: &'static str,
    /// Link time, seconds since 1970, unless `reproducible`.
    pub linked: u32,
    /// A reproducible build: `linked` is a hash of the contents, not a time
    /// (most of Windows' own files).
    pub reproducible: bool,
    pub dll: bool,
    pub image_base: u64,
    /// Loaded anywhere (DYNAMIC_BASE).
    pub aslr: bool,
    pub high_entropy: bool,
    /// No execution from data pages (NX_COMPAT).
    pub dep: bool,
    /// Control Flow Guard.
    pub cfg: bool,
    /// Windows refuses to load it unless its signature holds.
    pub force_integrity: bool,
    pub dotnet: bool,
}

#[derive(Clone, Debug)]
pub struct FileFacts {
    pub path: Arc<str>,
    pub size: u64,
    /// Last written, as a FILETIME.
    pub modified: Option<i64>,
    pub sha256: Option<String>,
    pub signature: Signature,
    pub pe: Option<PeFacts>,
    pub took: Duration,
}

pub fn file_facts(path: &str) -> Result<FileFacts, String> {
    let started = Instant::now();
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let modified = meta.modified().ok().and_then(|t| {
        let unix = t.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(unix.as_nanos() as i64 / 100 + 116_444_736_000_000_000)
    });
    let pe = std::fs::File::open(path).ok().and_then(|mut f| {
        let mut head = vec![0u8; 4096];
        let n = f.read(&mut head).ok()?;
        head.truncate(n);
        let mut pe = pe_facts(&head)?;
        pe.reproducible = reproducible(&mut f, &head);
        Some(pe)
    });
    Ok(FileFacts {
        path: Arc::from(path),
        size: meta.len(),
        modified,
        sha256: sha256(path),
        signature: signature(path),
        pe,
        took: started.elapsed(),
    })
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

/// Reads the facts out of the first page of a PE file.
pub fn pe_facts(d: &[u8]) -> Option<PeFacts> {
    if d.get(0..2)? != b"MZ" {
        return None;
    }
    let nt = u32_at(d, 0x3C)? as usize;
    if d.get(nt..nt + 4)? != b"PE\0\0" {
        return None;
    }
    let file = nt + 4;
    let machine = match u16_at(d, file)? {
        0x8664 => "x64",
        0x14C => "x86",
        0xAA64 => "ARM64",
        0x1C4 => "ARM",
        _ => "other",
    };
    let linked = u32_at(d, file + 4)?;
    let characteristics = u16_at(d, file + 18)?;
    let optional = file + 20;
    let pe32_plus = match u16_at(d, optional)? {
        0x20B => true,
        0x10B => false,
        _ => return None,
    };
    let image_base = if pe32_plus {
        u64_at(d, optional + 24)?
    } else {
        u32_at(d, optional + 28)? as u64
    };
    let dll_characteristics = u16_at(d, optional + 70)?;
    // The CLR runtime header is data directory 14.
    let directories = optional + if pe32_plus { 112 } else { 96 };
    let dotnet = u32_at(d, directories + 14 * 8)
        .zip(u32_at(d, optional + if pe32_plus { 108 } else { 92 }))
        .is_some_and(|(rva, count)| count > 14 && rva != 0);
    Some(PeFacts {
        machine,
        linked,
        reproducible: false,
        dll: characteristics & 0x2000 != 0,
        image_base,
        high_entropy: dll_characteristics & 0x0020 != 0,
        aslr: dll_characteristics & 0x0040 != 0,
        force_integrity: dll_characteristics & 0x0080 != 0,
        dep: dll_characteristics & 0x0100 != 0,
        cfg: dll_characteristics & 0x4000 != 0,
        dotnet,
    })
}

/// Whether the debug directory has an IMAGE_DEBUG_TYPE_REPRO entry.
fn reproducible(file: &mut std::fs::File, head: &[u8]) -> bool {
    use std::io::{Seek, SeekFrom};
    let entry = || -> Option<(u64, usize)> {
        let nt = u32_at(head, 0x3C)? as usize;
        let file_header = nt + 4;
        let sections = u16_at(head, file_header + 2)? as usize;
        let optional = file_header + 20;
        let optional_size = u16_at(head, file_header + 16)? as usize;
        let pe32_plus = u16_at(head, optional)? == 0x20B;
        let directories = optional + if pe32_plus { 112 } else { 96 };
        let (rva, size) = (
            u32_at(head, directories + 6 * 8)?,
            u32_at(head, directories + 6 * 8 + 4)?,
        );
        if rva == 0 || size == 0 {
            return None;
        }
        // Where that address lies in the file.
        let table = optional + optional_size;
        (0..sections).find_map(|i| {
            let at = table + i * 40;
            let virtual_size = u32_at(head, at + 8)?;
            let address = u32_at(head, at + 12)?;
            let raw = u32_at(head, at + 20)?;
            (rva >= address && rva < address + virtual_size.max(1))
                .then(|| ((raw + rva - address) as u64, (size as usize).min(28 * 32)))
        })
    };
    let Some((offset, size)) = entry() else {
        return false;
    };
    let mut buf = vec![0u8; size];
    if file.seek(SeekFrom::Start(offset)).is_err() || file.read_exact(&mut buf).is_err() {
        return false;
    }
    // IMAGE_DEBUG_DIRECTORY entries are 28 bytes; Type is at 12.
    buf.chunks_exact(28)
        .any(|d| u32::from_le_bytes([d[12], d[13], d[14], d[15]]) == 16)
}

/// SHA-256 of the whole file, as VirusTotal and friends know it.
pub fn sha256(path: &str) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut alg = BCRYPT_ALG_HANDLE::default();
    let status = unsafe {
        BCryptOpenAlgorithmProvider(
            &mut alg,
            BCRYPT_SHA256_ALGORITHM,
            PCWSTR::null(),
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
    };
    if status.is_err() {
        return None;
    }
    let mut hash = BCRYPT_HASH_HANDLE::default();
    let result = (|| {
        unsafe { BCryptCreateHash(alg, &mut hash, None, None, 0) }
            .ok()
            .ok()?;
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = file.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            unsafe { BCryptHashData(hash, &buf[..n], 0) }.ok().ok()?;
        }
        let mut digest = [0u8; 32];
        unsafe { BCryptFinishHash(hash, &mut digest, 0) }
            .ok()
            .ok()?;
        Some(digest.iter().map(|b| format!("{b:02x}")).collect())
    })();
    unsafe {
        if !hash.is_invalid() {
            let _ = BCryptDestroyHash(hash);
        }
        let _ = BCryptCloseAlgorithmProvider(alg, 0);
    }
    result
}

/// Asks WinVerifyTrust about `data`; on success, also who signed. Always
/// closes the state it opened.
fn verify(data: &mut WINTRUST_DATA) -> (i32, Option<String>) {
    let mut action: GUID = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    data.dwStateAction = WTD_STATEACTION_VERIFY;
    let status = unsafe {
        WinVerifyTrust(
            HWND::default(),
            &mut action,
            (data as *mut WINTRUST_DATA).cast(),
        )
    };
    let mut signer = None;
    if status == 0 {
        unsafe {
            let provider = WTHelperProvDataFromStateData(data.hWVTStateData);
            if !provider.is_null() {
                let sgnr = WTHelperGetProvSignerFromChain(provider, 0, false, 0);
                if !sgnr.is_null() && (*sgnr).csCertChain > 0 && !(*sgnr).pasCertChain.is_null() {
                    let cert = (*(*sgnr).pasCertChain).pCert;
                    let mut name = [0u16; 256];
                    let n = CertGetNameStringW(
                        cert,
                        CERT_NAME_SIMPLE_DISPLAY_TYPE,
                        0,
                        None,
                        Some(&mut name),
                    );
                    if n > 1 {
                        signer = Some(String::from_utf16_lossy(&name[..n as usize - 1]));
                    }
                }
            }
        }
    }
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            HWND::default(),
            &mut action,
            (data as *mut WINTRUST_DATA).cast(),
        )
    };
    (status, signer)
}

fn trust_data() -> WINTRUST_DATA {
    WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_REVOCATION_CHECK_NONE,
        ..Default::default()
    }
}

const TRUST_E_NOSIGNATURE: i32 = 0x800B_0100_u32 as i32;
const TRUST_E_SUBJECT_FORM_UNKNOWN: i32 = 0x800B_0003_u32 as i32;

pub fn signature(path: &str) -> Signature {
    let path_w = wide(path);
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(path_w.as_ptr()),
        ..Default::default()
    };
    let mut data = trust_data();
    data.dwUnionChoice = WTD_CHOICE_FILE;
    data.Anonymous = WINTRUST_DATA_0 { pFile: &mut file };
    let (status, signer) = verify(&mut data);
    match status {
        0 => Signature::Signed {
            signer,
            catalog: false,
        },
        TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN => match catalog_signature(path) {
            Some(signed) => signed,
            None => Signature::Unsigned,
        },
        e => Signature::Invalid(
            windows::core::Error::from_hresult(windows::core::HRESULT(e)).message(),
        ),
    }
}

/// Windows signs most of its own files in catalogs: the file's hash is
/// listed in a signed .cat file.
fn catalog_signature(path: &str) -> Option<Signature> {
    let file = std::fs::File::open(path).ok()?;
    let handle = HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(&file));
    let mut admin = 0isize;
    unsafe { CryptCATAdminAcquireContext2(&mut admin, None, BCRYPT_SHA256_ALGORITHM, None, None) }
        .ok()?;
    let result = (|| {
        let mut size = 0u32;
        let _ =
            unsafe { CryptCATAdminCalcHashFromFileHandle2(admin, handle, &mut size, None, None) };
        if size == 0 || size > 256 {
            return None;
        }
        let mut hash = vec![0u8; size as usize];
        unsafe {
            CryptCATAdminCalcHashFromFileHandle2(
                admin,
                handle,
                &mut size,
                Some(hash.as_mut_ptr()),
                None,
            )
        }
        .ok()?;
        let catalog = unsafe { CryptCATAdminEnumCatalogFromHash(admin, &hash, None, None) };
        if catalog == 0 {
            return None;
        }
        let mut info = CATALOG_INFO {
            cbStruct: size_of::<CATALOG_INFO>() as u32,
            wszCatalogFile: [0; 260],
        };
        let found = unsafe { CryptCATCatalogInfoFromContext(catalog, &mut info, 0) }.is_ok();
        let signed = found.then(|| {
            let tag = wide(&hash.iter().map(|b| format!("{b:02X}")).collect::<String>());
            let path_w = wide(path);
            let mut member = WINTRUST_CATALOG_INFO {
                cbStruct: size_of::<WINTRUST_CATALOG_INFO>() as u32,
                pcwszCatalogFilePath: PCWSTR(info.wszCatalogFile.as_ptr()),
                pcwszMemberTag: PCWSTR(tag.as_ptr()),
                pcwszMemberFilePath: PCWSTR(path_w.as_ptr()),
                hMemberFile: handle,
                pbCalculatedFileHash: hash.as_mut_ptr(),
                cbCalculatedFileHash: size,
                hCatAdmin: admin,
                ..Default::default()
            };
            let mut data = trust_data();
            data.dwUnionChoice = WTD_CHOICE_CATALOG;
            data.Anonymous = WINTRUST_DATA_0 {
                pCatalog: &mut member,
            };
            match verify(&mut data) {
                (0, signer) => Signature::Signed {
                    signer,
                    catalog: true,
                },
                (e, _) => Signature::Invalid(
                    windows::core::Error::from_hresult(windows::core::HRESULT(e)).message(),
                ),
            }
        });
        let _ = unsafe { CryptCATAdminReleaseCatalogContext(admin, catalog, 0) };
        signed
    })();
    let _ = unsafe { CryptCATAdminReleaseContext(admin, 0) };
    drop(file);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system32(name: &str) -> String {
        let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        format!(r"{windir}\System32\{name}")
    }

    #[test]
    fn windows_files_are_signed_by_microsoft() {
        // notepad.exe carries its signature inside; kernel32.dll is in a
        // catalog on most builds. Either way, Microsoft signed it.
        for name in ["notepad.exe", "kernel32.dll"] {
            let facts = file_facts(&system32(name)).expect("facts");
            match &facts.signature {
                Signature::Signed { signer, .. } => {
                    assert!(
                        signer.as_deref().is_some_and(|s| s.contains("Microsoft")),
                        "{name}: {signer:?}"
                    );
                }
                other => panic!("{name}: {other:?}"),
            }
            let pe = facts.pe.expect("a PE file");
            assert_eq!(pe.machine, "x64");
            assert!(pe.aslr && pe.dep, "{name}: {pe:?}");
            // Windows builds its own files reproducibly.
            assert!(pe.reproducible, "{name}: {pe:?}");
            assert_eq!(facts.sha256.as_ref().map(|h| h.len()), Some(64));
        }
    }

    #[test]
    fn a_text_file_is_unsigned_and_not_pe() {
        let path = std::env::temp_dir().join("anabolic-facts-test.txt");
        std::fs::write(&path, b"hello").unwrap();
        let facts = file_facts(path.to_str().unwrap()).unwrap();
        assert_eq!(facts.signature, Signature::Unsigned);
        assert!(facts.pe.is_none());
        assert_eq!(
            facts.sha256.as_deref(),
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
        let _ = std::fs::remove_file(path);
    }
}
