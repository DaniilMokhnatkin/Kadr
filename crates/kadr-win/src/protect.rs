//! DPAPI текущего пользователя: без UI, без машинного ключа и без открытых временных файлов.
use std::io;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;

pub fn seal(data: &[u8]) -> io::Result<Vec<u8>> {
    crypt(data, true)
}
pub fn open(data: &[u8]) -> io::Result<Vec<u8>> {
    crypt(data, false)
}
fn crypt(data: &[u8], encrypt: bool) -> io::Result<Vec<u8>> {
    let len = u32::try_from(data.len()).map_err(io::Error::other)?;
    let input = CRYPT_INTEGER_BLOB { cbData: len, pbData: data.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if encrypt {
            CryptProtectData(&input, PCWSTR::null(), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        } else {
            CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        }
        .map_err(io::Error::other)?;
        let result = if output.cbData == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
        };
        // Расшифрованная копия DPAPI больше не нужна; память изображения остаётся у вызывающего.
        if !encrypt {
            for i in 0..output.cbData as usize {
                std::ptr::write_volatile(output.pbData.add(i), 0);
            }
        }
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn user_dpapi_round_trip_and_corruption_rejection() {
        let data = b"Kadr confidential screenshot fixture";
        let sealed = seal(data).unwrap();
        assert!(!sealed.windows(data.len()).any(|w| w == data));
        assert_eq!(open(&sealed).unwrap(), data);
        assert!(open(b"not a DPAPI blob").is_err());
    }
}
