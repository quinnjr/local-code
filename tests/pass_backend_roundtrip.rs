//! Round-trips a secret through the full pass-backed credential stack —
//! builder -> credential -> GPG-encrypted file in a password store — using
//! an ephemeral GPG key in a temp GNUPGHOME, so nothing touches the user's
//! real keyrings or store. Skips (successfully, with a notice) when no
//! `gpg` binary is on PATH; ubuntu-latest CI has one.

#![cfg(all(unix, not(target_os = "macos")))]

use keyring::credential::CredentialBuilderApi;
use local_code::config::pass_backend::PassCredentialBuilder;
use pass_sys::PasswordStore;

fn gpg_available() -> bool {
    std::process::Command::new("gpg")
        .arg("--version")
        .output()
        .is_ok()
}

#[test]
fn pass_credential_round_trips_through_a_real_gpg_store() {
    if !gpg_available() {
        eprintln!("skipping: no gpg binary on PATH");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();

    // Ephemeral GPG home (gpg refuses group/other-accessible homedirs).
    let gnupg_home = tmp.path().join("gnupg");
    std::fs::create_dir_all(&gnupg_home).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gnupg_home, std::fs::Permissions::from_mode(0o700)).unwrap();

    let status = std::process::Command::new("gpg")
        .arg("--homedir")
        .arg(&gnupg_home)
        .args([
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-generate-key",
            "local-code-test <local-code-test@example.com>",
            "default",
            "default",
            "never",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "ephemeral test key generation failed");

    let store_dir = tmp.path().join("store");
    let store = PasswordStore::with_store_dir(&store_dir).with_gpg_home(&gnupg_home);
    store.init(&["local-code-test@example.com"]).unwrap();

    let builder = PassCredentialBuilder::with_store(store);
    let cred = builder.build(None, "local-code", "secret:pass-rt").unwrap();

    // Round trip.
    cred.set_password("tok-pass-rt-1").unwrap();
    assert_eq!(cred.get_password().unwrap(), "tok-pass-rt-1");

    // On-disk layout matches pass: <store>/<service>/<user>.gpg …
    let entry_file = store_dir.join("local-code/secret:pass-rt.gpg");
    assert!(
        entry_file.exists(),
        "entry file not written where pass expects it"
    );

    // … and the file is ciphertext: the plaintext token never hits disk.
    let raw = std::fs::read(&entry_file).unwrap();
    assert!(
        !raw.windows(b"tok-pass-rt-1".len())
            .any(|w| w == b"tok-pass-rt-1"),
        "plaintext token found in the on-disk entry"
    );

    // Overwrite is an update, not an error.
    cred.set_password("tok-pass-rt-2").unwrap();
    assert_eq!(cred.get_password().unwrap(), "tok-pass-rt-2");

    // Delete, then reads map to NoEntry.
    cred.delete_credential().unwrap();
    assert!(matches!(
        cred.get_password().unwrap_err(),
        keyring::Error::NoEntry
    ));
}
