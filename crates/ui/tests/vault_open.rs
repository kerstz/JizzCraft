//! The opening logic of the lock screen, end to end and without a window:
//! a library encrypted by the same engine the CLI uses, start-up detection,
//! unlocking on a worker, a keyed query, and the refusals.
use jizzcraft_core::vault::KdfParams;
use jizzcraft_core::{Db, Error, Filter, vault_convert};
use jizzcraft_ui::root::{Fail, Startup, spawn_unlock, startup, unlock_blocking};
use std::time::Duration;
use zeroize::Zeroizing;

const PASS: &str = "e2e-passphrase-0123456789";
const CANARY: &str = "zz-canary-7f3a.png";

fn encrypted_library() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir(&media).unwrap();
    image::RgbImage::from_pixel(8, 8, image::Rgb([200, 100, 50]))
        .save(media.join(CANARY))
        .unwrap();
    let db_path = dir.path().join("library.db");
    {
        let mut db = Db::open(&db_path).unwrap();
        jizzcraft_core::scan::import_folder(&mut db, &media).unwrap();
    }
    let fast = KdfParams {
        m_cost: 19456,
        t_cost: 2,
        p_cost: 1,
    };
    vault_convert::enable(&db_path, PASS, fast).unwrap();
    (dir, db_path)
}

#[test]
fn encrypted_library_is_found_locked_and_opens_only_with_the_passphrase() {
    let (_dir, db_path) = encrypted_library();
    assert_eq!(startup(&db_path), Startup::Encrypted);

    // Without a key the engine refuses, and a keyless open never creates or
    // alters anything.
    assert!(matches!(Db::open(&db_path), Err(Error::Locked)));

    let wrong = unlock_blocking(&db_path, "not the passphrase at all");
    assert_eq!(wrong.err(), Some(Fail::Wrong));

    // The same through the worker used by the window.
    let rx = spawn_unlock(
        db_path.clone(),
        Zeroizing::new(PASS.to_owned()),
        Box::new(|| {}),
    );
    let unlocked = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("worker answered")
        .expect("right passphrase");
    assert!(unlocked.opener.key.is_some());
    assert!(unlocked.db.is_encrypted());
    assert!(!unlocked.read_only);
    let rows = unlocked
        .db
        .query(&Filter {
            include_missing: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1, "the scanned image is in the library");
    // Jobs open their own connections from the same opener.
    assert!(unlocked.opener.open().is_ok());
    assert!(format!("{unlocked:?}").contains("redacted"));
}

#[test]
fn missing_header_is_an_error_and_creates_nothing() {
    let (_dir, db_path) = encrypted_library();
    let header = jizzcraft_core::vault::header_path(&db_path);
    std::fs::remove_file(&header).unwrap();
    assert_eq!(startup(&db_path), Startup::Failed(Fail::Header));
    assert!(!header.exists());
    // Still the encrypted file, byte for byte a non-SQLite header.
    assert_eq!(
        jizzcraft_core::db::probe_format(&db_path).unwrap(),
        jizzcraft_core::db::FileFormat::Encrypted
    );
}
