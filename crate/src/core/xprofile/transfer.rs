use super::account::XProfileAccount;
use crate::core::extracted_fs::ExtractedFilesystem;

const ACCOUNT_FILE_NAME: &str = "Account";

fn find_account_index(entries: &[(String, u64)]) -> Result<usize, anyhow::Error> {
    entries
        .iter()
        .position(|(name, _)| name.eq_ignore_ascii_case(ACCOUNT_FILE_NAME))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "xprofile: no {ACCOUNT_FILE_NAME:?} file at package root - is this really a \
                 profile package?"
            )
        })
}

/// Locates `Account`, parses it, hands it to `mutate`, then re-obfuscates
/// and installs the result back into `fs` via `override_file`.
///
/// Returns the XUID (big-endian, matching the STFS header's Profile ID
/// field shape) after `mutate` ran, for the caller to feed back in as
/// `profile_id_override` on whichever write session re-signs the package.
///
/// This only touches the Account file bytes and the in-memory filesystem
/// view, not the STFS header.
///
/// # Errors
///
/// If there's no `Account` entry at the package root, or if parsing it
/// fails (see `XProfileAccount::parse`).
pub(crate) fn transfer_account(
    fs: &mut ExtractedFilesystem,
    mutate: impl FnOnce(&mut XProfileAccount),
) -> Result<[u8; 8], anyhow::Error> {
    let entries = fs.file_entries();
    let idx = find_account_index(&entries)?;
    let size = usize::try_from(entries[idx].1)
        .map_err(|e| anyhow::anyhow!("xprofile: Account file too large for this platform: {e}"))?;
    let mut raw = vec![0u8; size];
    fs.read_file_range(idx, 0, &mut raw)?;
    let mut account = XProfileAccount::parse(&raw)?;
    mutate(&mut account);
    let new_profile_id = account.xuid_online.to_be_bytes();
    fs.override_file(ACCOUNT_FILE_NAME, account.to_array())?;
    Ok(new_profile_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_account_at_root() {
        let entries = vec![
            ("default.xex".to_string(), 100u64),
            ("Account".to_string(), 0x190u64),
        ];
        assert_eq!(find_account_index(&entries).unwrap(), 1);
    }

    #[test]
    fn matches_account_case_insensitively() {
        let entries = vec![("account".to_string(), 0x190u64)];
        assert_eq!(find_account_index(&entries).unwrap(), 0);
    }

    #[test]
    fn errors_when_no_account_file_present() {
        let entries = vec![("default.xex".to_string(), 100u64)];
        let err = find_account_index(&entries).unwrap_err();
        assert!(
            err.to_string()
                .contains("no \"Account\" file at package root")
        );
    }

    #[test]
    fn errors_on_empty_listing() {
        let entries: Vec<(String, u64)> = vec![];
        assert!(find_account_index(&entries).is_err());
    }
}
