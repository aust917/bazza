use super::*;
use crate::database_path;

#[test]
fn exact_credential_is_library_and_connection_bound_across_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = database_path(root.path());
    let store = Store::open(&path).unwrap();
    let library = Uuid::new_v4();
    let other_library = Uuid::new_v4();
    let issued = store
        .service_issue_external_read_grant(library, "Клиент MCP")
        .unwrap();
    let another = store
        .service_issue_external_read_grant(library, "Other client")
        .unwrap();
    let id = issued.grant.connection_id;
    let secret = issued.secret.expose();

    let persisted: [u8; 32] = store
        .conn
        .lock()
        .query_row(
            "SELECT credential_digest FROM service_external_read_grants WHERE connection_id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted, issued.secret.digest(library, id));
    assert!(!format!("{issued:?}").contains(secret));
    let serialized = serde_json::to_string(&issued.grant).unwrap();
    assert!(!serialized.contains(secret));
    assert!(!serialized.contains("digest"));

    store
        .service_authorize_external_read(library, id, &issued.secret)
        .unwrap();
    for (wrong_library, wrong_connection, wrong_secret) in [
        (other_library, id, &issued.secret),
        (library, another.grant.connection_id, &issued.secret),
        (library, id, &another.secret),
        (Uuid::nil(), id, &issued.secret),
        (library, Uuid::nil(), &issued.secret),
    ] {
        assert!(matches!(
            store.service_authorize_external_read(wrong_library, wrong_connection, wrong_secret),
            Err(DataError::Forbidden)
        ));
    }

    let reloaded = ExternalReadSecret::parse(secret.to_owned()).unwrap();
    drop(store);
    assert!(!std::fs::read(&path)
        .unwrap()
        .windows(secret.len())
        .any(|window| window == secret.as_bytes()));
    let store = Store::open(&path).unwrap();
    store
        .service_authorize_external_read(library, id, &reloaded)
        .unwrap();
    assert!(matches!(
        store.service_revoke_external_read_grant(other_library, id),
        Err(DataError::NotFound)
    ));
    let revoked = store
        .service_revoke_external_read_grant(library, id)
        .unwrap();
    assert!(revoked.revoked_at.is_some());
    assert_eq!(
        store
            .service_revoke_external_read_grant(library, id)
            .unwrap(),
        revoked
    );
    assert!(matches!(
        store.service_authorize_external_read(library, id, &reloaded),
        Err(DataError::Forbidden)
    ));
    assert!(store
        .conn
        .lock()
        .execute(
            "UPDATE service_external_read_grants SET revoked_at=NULL WHERE connection_id=?1",
            [id.to_string()]
        )
        .is_err());
    drop(store);
    let store = Store::open(&path).unwrap();
    assert!(matches!(
        store.service_authorize_external_read(library, id, &reloaded),
        Err(DataError::Forbidden)
    ));
    assert!(store
        .service_list_external_read_grants(library, None, 100)
        .unwrap()
        .contains(&revoked));
}

#[test]
fn local_grant_metadata_pages_are_bounded_and_retain_revocation() {
    let store = Store::open_in_memory().unwrap();
    let library = Uuid::new_v4();
    let other_library = Uuid::new_v4();
    let first = store
        .service_issue_external_read_grant(library, "First")
        .unwrap()
        .grant;
    let second = store
        .service_issue_external_read_grant(library, "Second")
        .unwrap()
        .grant;
    let other = store
        .service_issue_external_read_grant(other_library, "Another library")
        .unwrap()
        .grant;
    let revoked = store
        .service_revoke_external_read_grant(library, first.connection_id)
        .unwrap();
    let mut expected = [revoked, second];
    expected.sort_by_key(|grant| grant.connection_id);
    let page = store
        .service_list_external_read_grants(library, None, 1)
        .unwrap();
    assert_eq!(page, expected[..1]);
    let next = store
        .service_list_external_read_grants(library, Some(page[0].connection_id), 1)
        .unwrap();
    assert_eq!(next, expected[1..]);
    assert!(store
        .service_list_external_read_grants(library, Some(next[0].connection_id), 1)
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .service_list_external_read_grants(other_library, None, 100)
            .unwrap(),
        vec![other]
    );
    assert!(store
        .service_list_external_read_grants(library, None, 101)
        .is_err());
    assert!(store
        .service_list_external_read_grants(library, Some(Uuid::nil()), 100)
        .is_err());
    for label in ["", "   ", "\x1b", "\0", &"Ж".repeat(65)] {
        assert!(store
            .service_issue_external_read_grant(library, label)
            .is_err());
    }
    for secret in ["", "not-a-credential", &"A".repeat(64), &"0".repeat(65)] {
        assert!(ExternalReadSecret::parse(secret.into()).is_err());
    }
}
