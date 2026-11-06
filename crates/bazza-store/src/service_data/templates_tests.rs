use super::*;

const OWNER: &str = "private-desktop-fixture";
fn fixture() -> Store {
    Store::open_in_memory().unwrap()
}
fn call(store: &Store, r: DesktopTemplatesRequest) -> DesktopTemplatesReply {
    store.service_desktop_templates(OWNER, &r).unwrap()
}
fn describe(store: &Store, id: &str) -> TemplateHeader {
    let DesktopTemplatesReply::Describe(h) = call(
        store,
        DesktopTemplatesRequest::Describe {
            template_id: id.into(),
        },
    ) else {
        panic!("header")
    };
    h
}
fn begin(id: &str, body: &str, key: &str) -> TemplateBeginWrite {
    TemplateBeginWrite {
        upload_id: Uuid::new_v4(),
        target: TemplateWriteTarget::New {
            requested_id: id.into(),
        },
        name: "  Мой шаблон  ".into(),
        description: "Fixture description".into(),
        total_bytes: body.len() as u64,
        sha256: sha(body.as_bytes()),
        idempotency_key: key.into(),
    }
}
fn append(store: &Store, r: &TemplateBeginWrite, body: &str) {
    let mut offset = 0;
    let mut seq = 1;
    while offset < body.len() {
        let mut end = (offset + 4096).min(body.len());
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        let text = body[offset..end].to_owned();
        call(
            store,
            DesktopTemplatesRequest::AppendWrite(TemplateAppendWrite {
                upload_id: r.upload_id,
                sequence: seq,
                byte_offset: offset as u64,
                sha256: sha(text.as_bytes()),
                text,
                idempotency_key: format!("append-{}-{seq}", r.upload_id),
            }),
        );
        offset = end;
        seq += 1;
    }
}
fn commit(store: &Store, r: &TemplateBeginWrite, key: &str) -> TemplateReceipt {
    let DesktopTemplatesReply::Mutation(receipt) = call(
        store,
        DesktopTemplatesRequest::CommitWrite(TemplateUploadRequest {
            upload_id: r.upload_id,
            idempotency_key: key.into(),
        }),
    ) else {
        panic!("saved receipt")
    };
    receipt
}
fn save(store: &Store, id: &str, body: &str, key: &str) -> TemplateReceipt {
    let r = begin(id, body, key);
    call(store, DesktopTemplatesRequest::BeginWrite(r.clone()));
    append(store, &r, body);
    commit(store, &r, &format!("commit-{key}"))
}
fn legacy(store: &Store, id: &str, name: &str, description: &str, body: &str, builtin: bool) {
    store
        .upsert_template(&crate::Template {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            body: body.into(),
            builtin,
        })
        .unwrap();
    store
        .conn
        .lock()
        .execute(
            "INSERT INTO service_template_state VALUES(?1,?2,?3,'present',?4)",
            params![id, revision(), revision(), builtin],
        )
        .unwrap();
}
fn collect(store: &Store, id: &str, field: TemplateField) -> String {
    let h = describe(store, id);
    let mut cursor = None;
    let mut text = String::new();
    let mut count = 0;
    loop {
        let r = DesktopTemplatesRequest::ReadField(TemplateReadRequest {
            template_id: id.into(),
            expected_revision: h.revision.clone(),
            field,
            cursor,
            max_bytes: 2048,
        });
        let reply = store.service_desktop_templates(OWNER, &r).unwrap();
        assert!(reply.valid_for(&r));
        let DesktopTemplatesReply::ReadField(page) = reply else {
            panic!("field")
        };
        assert_eq!(page.byte_offset, text.len() as u64);
        text.push_str(&page.text);
        cursor = page.next;
        count += 1;
        assert!(count < 1000);
        if cursor.is_none() {
            return text;
        }
    }
}

#[test]
fn upload_preserves_unicode_ids_whitespace_nul_and_marker_split_between_chunks() {
    let store = fixture();
    let chunks = ["原文\0{{trans", "cript}}\nрусский 🐙"];
    let body = chunks.concat();
    let mut r = begin(" шаблон / 日本語 ", &body, "split");
    r.name = "  Мой\0шаблон  ".into();
    r.description = "Описание\0日本語 🐙".into();
    call(&store, DesktopTemplatesRequest::BeginWrite(r.clone()));
    let mut offset = 0;
    for (index, text) in chunks.iter().enumerate() {
        let request = DesktopTemplatesRequest::AppendWrite(TemplateAppendWrite {
            upload_id: r.upload_id,
            sequence: index as u32 + 1,
            byte_offset: offset,
            text: (*text).into(),
            sha256: sha(text.as_bytes()),
            idempotency_key: format!("split-{index}"),
        });
        let first = call(&store, request.clone());
        let second = call(&store, request);
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::to_value(second).unwrap()
        );
        offset += text.len() as u64;
    }
    let receipt = commit(&store, &r, "split-commit");
    assert_eq!(receipt.template_id.as_deref(), Some(" шаблон / 日本語 "));
    let saved = store.get_template(" шаблон / 日本語 ").unwrap().unwrap();
    assert_eq!(saved.body, body);
    assert_eq!(saved.name, r.name);
    assert_eq!(saved.description, r.description);
    assert!(!saved.builtin);
    assert_eq!(collect(&store, &saved.id, TemplateField::Body), body);
    assert_eq!(collect(&store, &saved.id, TemplateField::Name), r.name);
    assert_eq!(
        collect(&store, &saved.id, TemplateField::Description),
        r.description
    );
    assert_eq!(
        store
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM service_template_chunks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(store
        .conn
        .lock()
        .query_row(
            "SELECT name IS NULL AND description IS NULL FROM service_template_uploads WHERE id=?1",
            [r.upload_id.to_string()],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
    store
        .conn
        .lock()
        .execute("DELETE FROM service_idempotency", [])
        .unwrap();
    assert_eq!(commit(&store, &r, "after-retention"), receipt);
}

#[test]
fn legacy_large_fields_page_exactly_and_giant_body_exceeds_new_write_limit_without_loading_it() {
    let store = fixture();
    let name = "名".repeat(600);
    let description = "説明\0".repeat(600);
    let small = "я🙂\0".repeat(2000);
    legacy(&store, "legacy.large", &name, &description, &small, false);
    let h = describe(&store, "legacy.large");
    assert!(h.name_preview.len() < h.name_bytes as usize);
    assert!(h.description_preview.len() < h.description_bytes as usize);
    assert_eq!(collect(&store, &h.id, TemplateField::Name), name);
    assert_eq!(
        collect(&store, &h.id, TemplateField::Description),
        description
    );
    assert_eq!(collect(&store, &h.id, TemplateField::Body), small);
    legacy(&store, "empty", "Empty", "", "", false);
    assert_eq!(collect(&store, "empty", TemplateField::Body), "");
    let giant = format!("{}{{{{transcript}}}}\0尾", "я".repeat(4_200_000));
    legacy(&store, "giant", "Giant", "", &giant, false);
    let header = describe(&store, "giant");
    assert!(header.body_bytes > MAX_TEMPLATE_BODY_BYTES);
    let mut cursor = None;
    let mut offset = 0;
    for _ in 0..3 {
        let request = DesktopTemplatesRequest::ReadField(TemplateReadRequest {
            template_id: "giant".into(),
            expected_revision: header.revision.clone(),
            field: TemplateField::Body,
            cursor,
            max_bytes: 2048,
        });
        let reply = call(&store, request.clone());
        assert!(reply.valid_for(&request));
        let DesktopTemplatesReply::ReadField(page) = reply else {
            panic!()
        };
        assert_eq!(page.byte_offset, offset);
        assert!(!page.text.is_empty());
        offset += page.text.len() as u64;
        cursor = page.next;
    }
    let mut tail = cursor.unwrap();
    tail.byte_offset = (giant.len() - "{{transcript}}\0尾".len()) as u64;
    let DesktopTemplatesReply::ReadField(page) = call(
        &store,
        DesktopTemplatesRequest::ReadField(TemplateReadRequest {
            template_id: "giant".into(),
            expected_revision: header.revision,
            field: TemplateField::Body,
            cursor: Some(tail),
            max_bytes: 2048,
        }),
    ) else {
        panic!()
    };
    assert_eq!(page.text, "{{transcript}}\0尾");
    assert!(page.next.is_none());
}

#[test]
fn durable_intent_and_retired_upload_never_generate_a_new_template_id() {
    let store = fixture();
    let body = "{{transcript}}";
    let r = begin(" \t", body, "generated");
    let DesktopTemplatesReply::Upload(first) =
        call(&store, DesktopTemplatesRequest::BeginWrite(r.clone()))
    else {
        panic!()
    };
    assert!(Uuid::parse_str(&first.template_id).is_ok());
    append(&store, &r, body);
    call(
        &store,
        DesktopTemplatesRequest::AbortWrite(TemplateUploadRequest {
            upload_id: r.upload_id,
            idempotency_key: "abort-generated".into(),
        }),
    );
    store
        .conn
        .lock()
        .execute("DELETE FROM service_idempotency", [])
        .unwrap();
    let DesktopTemplatesReply::Upload(retired) =
        call(&store, DesktopTemplatesRequest::BeginWrite(r.clone()))
    else {
        panic!()
    };
    assert_eq!(retired.template_id, first.template_id);
    assert_eq!(retired.status, TemplateUploadStatus::Aborted);
    assert_eq!(
        describe(&store, &first.template_id).state,
        TemplateDocumentState::Absent
    );
    let mut changed = r.clone();
    changed.description = "different".into();
    assert!(matches!(
        store.service_desktop_templates(OWNER, &DesktopTemplatesRequest::BeginWrite(changed)),
        Err(DataError::IdempotencyConflict)
    ));
    let mut reused_key = r.clone();
    reused_key.upload_id = Uuid::new_v4();
    assert!(matches!(
        store.service_desktop_templates(OWNER, &DesktopTemplatesRequest::BeginWrite(reused_key)),
        Err(DataError::IdempotencyConflict)
    ));
    assert!(matches!(
        store.service_desktop_templates("another-owner", &DesktopTemplatesRequest::BeginWrite(r)),
        Err(DataError::Forbidden)
    ));
    assert_eq!(
        store
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM service_template_chunks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn append_order_digest_key_and_serialized_escape_budgets_are_checked() {
    let store = fixture();
    let body = "{{transcript}}";
    let r = begin("chunks", body, "chunks");
    call(&store, DesktopTemplatesRequest::BeginWrite(r.clone()));
    let good = TemplateAppendWrite {
        upload_id: r.upload_id,
        sequence: 1,
        byte_offset: 0,
        text: body.into(),
        sha256: sha(body.as_bytes()),
        idempotency_key: "chunk-key".into(),
    };
    let mut wrong = good.clone();
    wrong.sequence = 2;
    assert!(store
        .service_desktop_templates(OWNER, &DesktopTemplatesRequest::AppendWrite(wrong))
        .is_err());
    let mut wrong = good.clone();
    wrong.sha256 = "0".repeat(64);
    assert!(store
        .service_desktop_templates(OWNER, &DesktopTemplatesRequest::AppendWrite(wrong))
        .is_err());
    call(&store, DesktopTemplatesRequest::AppendWrite(good.clone()));
    let mut wrong = good;
    wrong.text = "changed".into();
    wrong.sha256 = sha(wrong.text.as_bytes());
    assert!(matches!(
        store.service_desktop_templates(OWNER, &DesktopTemplatesRequest::AppendWrite(wrong)),
        Err(DataError::IdempotencyConflict)
    ));
    let text = "\0".repeat(6000);
    let oversized = DesktopTemplatesRequest::AppendWrite(TemplateAppendWrite {
        upload_id: r.upload_id,
        sequence: 2,
        byte_offset: body.len() as u64,
        sha256: sha(text.as_bytes()),
        text,
        idempotency_key: "oversize".into(),
    });
    assert!(!oversized.valid());
    assert!(store.service_desktop_templates(OWNER, &oversized).is_err());
    let mut too_big = begin("large", "", "too-large");
    too_big.total_bytes = MAX_TEMPLATE_BODY_BYTES + 1;
    assert!(!DesktopTemplatesRequest::BeginWrite(too_big).valid());
    let empty = begin("missing-marker", "no marker", "missing-marker");
    call(&store, DesktopTemplatesRequest::BeginWrite(empty.clone()));
    append(&store, &empty, "no marker");
    assert!(matches!(
        store.service_desktop_templates(
            OWNER,
            &DesktopTemplatesRequest::CommitWrite(TemplateUploadRequest {
                upload_id: empty.upload_id,
                idempotency_key: "invalid-commit".into()
            })
        ),
        Err(DataError::Invalid("template_transcript"))
    ));
    assert_eq!(
        describe(&store, "missing-marker").state,
        TemplateDocumentState::Absent
    );
}

#[test]
fn deletion_restoration_and_retention_prevent_aba_stale_upload_or_cursor() {
    let store = fixture();
    let saved = save(&store, "ABA / 模板", "{{transcript}} original", "first");
    let original = describe(&store, "ABA / 模板");
    let mut stale = begin(&original.id, "{{transcript}} stale", "stale");
    stale.target = TemplateWriteTarget::Existing {
        template_id: original.id.clone(),
        expected_revision: original.revision.clone(),
    };
    call(&store, DesktopTemplatesRequest::BeginWrite(stale.clone()));
    append(&store, &stale, "{{transcript}} stale");
    let deletion = TemplateDeleteRequest {
        template_id: original.id.clone(),
        expected_revision: original.revision.clone(),
        idempotency_key: "delete".into(),
    };
    let deleted = call(&store, DesktopTemplatesRequest::Delete(deletion.clone()));
    let tombstone = describe(&store, &original.id);
    assert_eq!(tombstone.state, TemplateDocumentState::Deleted);
    assert!(matches!(
        store.service_desktop_templates(
            OWNER,
            &DesktopTemplatesRequest::BeginWrite(begin(
                &original.id,
                "{{transcript}}",
                "new-reuse"
            ))
        ),
        Err(DataError::Conflict { .. })
    ));
    let mut restore = begin(&original.id, "{{transcript}} restored", "restore");
    restore.target = TemplateWriteTarget::Existing {
        template_id: original.id.clone(),
        expected_revision: tombstone.revision,
    };
    call(&store, DesktopTemplatesRequest::BeginWrite(restore.clone()));
    append(&store, &restore, "{{transcript}} restored");
    let restored = commit(&store, &restore, "restore-commit");
    assert_ne!(restored.generation, saved.generation);
    assert_eq!(
        serde_json::to_value(call(
            &store,
            DesktopTemplatesRequest::Delete(deletion.clone())
        ))
        .unwrap(),
        serde_json::to_value(deleted).unwrap()
    );
    store
        .conn
        .lock()
        .execute("DELETE FROM service_idempotency", [])
        .unwrap();
    assert!(matches!(
        store.service_desktop_templates(OWNER, &DesktopTemplatesRequest::Delete(deletion)),
        Err(DataError::Conflict { .. })
    ));
    assert!(matches!(
        store.service_desktop_templates(
            OWNER,
            &DesktopTemplatesRequest::CommitWrite(TemplateUploadRequest {
                upload_id: stale.upload_id,
                idempotency_key: "late".into()
            })
        ),
        Err(DataError::Conflict { .. })
    ));
    assert!(matches!(
        store.service_desktop_templates(
            OWNER,
            &DesktopTemplatesRequest::ReadField(TemplateReadRequest {
                template_id: original.id,
                expected_revision: original.revision,
                field: TemplateField::Body,
                cursor: None,
                max_bytes: 2048
            })
        ),
        Err(DataError::Conflict { .. })
    ));
}

#[test]
fn seed_preserves_custom_collision_and_edited_builtin_and_deletes_are_truthful_noops() {
    let store = fixture();
    legacy(
        &store,
        "general",
        "Custom general",
        "exact description",
        "{{transcript}} USER",
        false,
    );
    let catalog = catalog_on(&store.conn.lock()).unwrap();
    call(
        &store,
        DesktopTemplatesRequest::SeedMissingBuiltins(TemplateSeedRequest {
            expected_catalog_revision: catalog,
            idempotency_key: "seed".into(),
        }),
    );
    assert_eq!(
        store.get_template("general").unwrap().unwrap().body,
        "{{transcript}} USER"
    );
    assert!(!describe(&store, "general").builtin);
    let builtin = describe(&store, "sales-call");
    let mut edit = begin(&builtin.id, "{{transcript}} edited", "edit-builtin");
    edit.target = TemplateWriteTarget::Existing {
        template_id: builtin.id.clone(),
        expected_revision: builtin.revision,
    };
    call(&store, DesktopTemplatesRequest::BeginWrite(edit.clone()));
    append(&store, &edit, "{{transcript}} edited");
    commit(&store, &edit, "edit-builtin-commit");
    let builtin = describe(&store, "sales-call");
    assert!(builtin.builtin);
    let before = store.service_changes(0, 100).unwrap().len();
    let DesktopTemplatesReply::Mutation(protected) = call(
        &store,
        DesktopTemplatesRequest::Delete(TemplateDeleteRequest {
            template_id: builtin.id,
            expected_revision: builtin.revision,
            idempotency_key: "delete-builtin".into(),
        }),
    ) else {
        panic!()
    };
    assert_eq!(protected.effect, TemplateEffect::BuiltinProtectedNoOp);
    assert!(protected.change_sequence.is_none());
    let absent = describe(&store, "missing");
    let DesktopTemplatesReply::Mutation(noop) = call(
        &store,
        DesktopTemplatesRequest::Delete(TemplateDeleteRequest {
            template_id: absent.id,
            expected_revision: absent.revision,
            idempotency_key: "delete-missing".into(),
        }),
    ) else {
        panic!()
    };
    assert_eq!(noop.effect, TemplateEffect::AlreadyAbsent);
    assert!(noop.change_sequence.is_none());
    assert_eq!(store.service_changes(0, 100).unwrap().len(), before);
    let catalog = catalog_on(&store.conn.lock()).unwrap();
    let DesktopTemplatesReply::Mutation(seed) = call(
        &store,
        DesktopTemplatesRequest::SeedMissingBuiltins(TemplateSeedRequest {
            expected_catalog_revision: catalog,
            idempotency_key: "seed-again".into(),
        }),
    ) else {
        panic!()
    };
    assert!(seed.inserted_ids.is_empty());
    assert!(seed.change_sequence.is_none());
    assert_eq!(
        store.get_template("sales-call").unwrap().unwrap().body,
        "{{transcript}} edited"
    );
}

#[test]
fn upload_expiry_purges_payload_and_count_and_reserved_quotas_are_bounded() {
    let store = fixture();
    let mut inputs = Vec::new();
    for i in 0..16 {
        let r = begin(
            &format!("quota-{i}"),
            "{{transcript}}",
            &format!("quota-{i}"),
        );
        call(&store, DesktopTemplatesRequest::BeginWrite(r.clone()));
        if i == 0 {
            append(&store, &r, "{{transcript}}");
        }
        inputs.push(r);
    }
    assert!(matches!(
        store.service_desktop_templates(
            OWNER,
            &DesktopTemplatesRequest::BeginWrite(begin("overflow", "{{transcript}}", "overflow"))
        ),
        Err(DataError::Busy)
    ));
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_template_uploads SET expires_at='2000-01-01T00:00:00Z'",
            [],
        )
        .unwrap();
    assert_eq!(store.service_expire_template_uploads(32).unwrap(), 16);
    let DesktopTemplatesReply::Upload(expired) = call(
        &store,
        DesktopTemplatesRequest::BeginWrite(inputs[0].clone()),
    ) else {
        panic!()
    };
    assert_eq!(expired.status, TemplateUploadStatus::Expired);
    assert_eq!(
        store
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM service_template_chunks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(store.conn.lock().query_row("SELECT COUNT(*) FROM service_template_uploads WHERE name IS NOT NULL OR description IS NOT NULL",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    for i in 0..8 {
        let mut r = begin(&format!("reserved-{i}"), "", &format!("reserved-{i}"));
        r.total_bytes = MAX_TEMPLATE_BODY_BYTES;
        call(&store, DesktopTemplatesRequest::BeginWrite(r));
    }
    let mut overflow = begin("reserved-overflow", "", "reserved-overflow");
    overflow.total_bytes = 1;
    assert!(matches!(
        store.service_desktop_templates(OWNER, &DesktopTemplatesRequest::BeginWrite(overflow)),
        Err(DataError::Busy)
    ));
}

#[test]
fn late_commit_failure_rolls_back_template_state_catalog_and_receipt_then_retries() {
    let store = fixture();
    let r = begin("rollback", "{{transcript}} exact", "rollback");
    call(&store, DesktopTemplatesRequest::BeginWrite(r.clone()));
    append(&store, &r, "{{transcript}} exact");
    let catalog = catalog_on(&store.conn.lock()).unwrap();
    store.conn.lock().execute_batch("CREATE TRIGGER fail_template_commit BEFORE INSERT ON service_changes WHEN NEW.entity_kind='template' BEGIN SELECT RAISE(ABORT,'synthetic late failure'); END;").unwrap();
    let request = DesktopTemplatesRequest::CommitWrite(TemplateUploadRequest {
        upload_id: r.upload_id,
        idempotency_key: "rollback-commit".into(),
    });
    assert!(store.service_desktop_templates(OWNER, &request).is_err());
    assert_eq!(
        describe(&store, "rollback").state,
        TemplateDocumentState::Absent
    );
    assert_eq!(catalog_on(&store.conn.lock()).unwrap(), catalog);
    assert_eq!(
        upload_on(&store.conn.lock(), r.upload_id, OWNER)
            .unwrap()
            .public
            .status,
        TemplateUploadStatus::Open
    );
    store
        .conn
        .lock()
        .execute_batch("DROP TRIGGER fail_template_commit")
        .unwrap();
    let result = call(&store, request);
    assert!(matches!(result, DesktopTemplatesReply::Mutation(_)));
    assert_eq!(
        store.get_template("rollback").unwrap().unwrap().body,
        "{{transcript}} exact"
    );
}

#[test]
fn catalog_and_field_cursors_bind_scope_revision_field_and_progress_without_read_writes() {
    let store = fixture();
    save(&store, "A", "{{transcript}} a", "a");
    save(&store, "B", "{{transcript}} b", "b");
    let before = store.service_changes(0, 100).unwrap().len();
    let request = DesktopTemplatesRequest::List(TemplateListRequest {
        cursor: None,
        limit: 1,
        max_bytes: 2048,
    });
    let DesktopTemplatesReply::List(page) = call(&store, request) else {
        panic!()
    };
    let mut cursor = page.next.unwrap();
    cursor.library_scope = "f".repeat(32);
    assert!(matches!(
        store.service_desktop_templates(
            OWNER,
            &DesktopTemplatesRequest::List(TemplateListRequest {
                cursor: Some(cursor),
                limit: 1,
                max_bytes: 2048
            })
        ),
        Err(DataError::StaleCursor)
    ));
    let h = describe(&store, "A");
    let bad = DesktopTemplatesRequest::ReadField(TemplateReadRequest {
        template_id: "A".into(),
        expected_revision: h.revision.clone(),
        field: TemplateField::Body,
        cursor: Some(TemplateReadCursor {
            library_scope: "f".repeat(32),
            template_id: "A".into(),
            revision: h.revision,
            generation: h.generation,
            field: TemplateField::Body,
            byte_offset: 0,
            max_bytes: 2048,
        }),
        max_bytes: 2048,
    });
    assert!(matches!(
        store.service_desktop_templates(OWNER, &bad),
        Err(DataError::StaleCursor)
    ));
    assert_eq!(store.service_changes(0, 100).unwrap().len(), before);
    legacy(&store, &"x".repeat(513), "bad ID", "", "legacy", false);
    let DesktopTemplatesReply::List(prefix) = call(
        &store,
        DesktopTemplatesRequest::List(TemplateListRequest {
            cursor: None,
            limit: 100,
            max_bytes: 65024,
        }),
    ) else {
        panic!()
    };
    assert_eq!(prefix.items.len(), 2);
    // A valid prefix is useful, but the invalid row must remain unconsumed.
    assert!(store
        .service_desktop_templates(
            OWNER,
            &DesktopTemplatesRequest::List(TemplateListRequest {
                cursor: prefix.next,
                limit: 100,
                max_bytes: 65024,
            }),
        )
        .is_err());
}

#[test]
fn a_valid_catalog_prefix_is_returned_before_an_unconsumed_invalid_legacy_id() {
    let store = fixture();
    legacy(&store, "valid", "A first", "", "{{transcript}}", false);
    legacy(
        &store,
        &"x".repeat(513),
        "Z invalid",
        "",
        "{{transcript}}",
        false,
    );
    let DesktopTemplatesReply::List(page) = call(
        &store,
        DesktopTemplatesRequest::List(TemplateListRequest {
            cursor: None,
            limit: 100,
            max_bytes: 2048,
        }),
    ) else {
        panic!("list");
    };
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, "valid");
    assert!(matches!(
        store.service_desktop_templates(
            OWNER,
            &DesktopTemplatesRequest::List(TemplateListRequest {
                cursor: page.next,
                limit: 100,
                max_bytes: 2048
            })
        ),
        Err(DataError::Invalid("legacy template identifier budget"))
    ));
}

#[test]
fn two_edits_keep_the_original_expected_revision_and_only_one_commit_wins() {
    let store = fixture();
    save(
        &store,
        "shared / шаблон",
        "{{transcript}} original",
        "original",
    );
    let header = describe(&store, "shared / шаблон");
    let mut first = begin(&header.id, "{{transcript}} first", "editor-first");
    first.target = TemplateWriteTarget::Existing {
        template_id: header.id.clone(),
        expected_revision: header.revision.clone(),
    };
    let mut second = begin(&header.id, "{{transcript}} second", "editor-second");
    second.target = first.target.clone();
    call(&store, DesktopTemplatesRequest::BeginWrite(first.clone()));
    call(&store, DesktopTemplatesRequest::BeginWrite(second.clone()));
    append(&store, &first, "{{transcript}} first");
    append(&store, &second, "{{transcript}} second");
    let winner = commit(&store, &first, "first-commit");
    let request = DesktopTemplatesRequest::CommitWrite(TemplateUploadRequest {
        upload_id: second.upload_id,
        idempotency_key: "second-commit".into(),
    });
    assert!(matches!(
        store.service_desktop_templates(OWNER, &request),
        Err(DataError::Conflict { .. })
    ));
    assert_eq!(
        store.get_template(&header.id).unwrap().unwrap().body,
        "{{transcript}} first"
    );
    assert_eq!(describe(&store, &header.id).revision, winner.revision);
    assert_eq!(describe(&store, &header.id).generation, header.generation);
    assert_eq!(
        upload_on(&store.conn.lock(), second.upload_id, OWNER)
            .unwrap()
            .public
            .status,
        TemplateUploadStatus::Open
    );
    call(
        &store,
        DesktopTemplatesRequest::AbortWrite(TemplateUploadRequest {
            upload_id: second.upload_id,
            idempotency_key: "abort-second".into(),
        }),
    );
    assert_eq!(
        store
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM service_template_chunks", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn append_keys_retain_only_fingerprints_across_commit_abort_expiry_and_other_uploads() {
    for terminal in [
        TemplateUploadStatus::Committed,
        TemplateUploadStatus::Aborted,
        TemplateUploadStatus::Expired,
    ] {
        let store = fixture();
        let body = "{{transcript}} PRIVATE_PAYLOAD\0exact";
        let upload = begin("retired", body, "begin-retired");
        call(&store, DesktopTemplatesRequest::BeginWrite(upload.clone()));
        let append = TemplateAppendWrite {
            upload_id: upload.upload_id,
            sequence: 1,
            byte_offset: 0,
            text: body.into(),
            sha256: sha(body.as_bytes()),
            idempotency_key: "global-append-key".into(),
        };
        call(&store, DesktopTemplatesRequest::AppendWrite(append.clone()));
        match terminal {
            TemplateUploadStatus::Committed => {
                commit(&store, &upload, "commit-retired");
            }
            TemplateUploadStatus::Aborted => {
                call(
                    &store,
                    DesktopTemplatesRequest::AbortWrite(TemplateUploadRequest {
                        upload_id: upload.upload_id,
                        idempotency_key: "abort-retired".into(),
                    }),
                );
            }
            TemplateUploadStatus::Expired => {
                store.conn.lock().execute("UPDATE service_template_uploads SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1",[upload.upload_id.to_string()]).unwrap();
                assert_eq!(store.service_expire_template_uploads(32).unwrap(), 1);
            }
            TemplateUploadStatus::Open => unreachable!(),
        }
        let DesktopTemplatesReply::Upload(replayed) =
            call(&store, DesktopTemplatesRequest::AppendWrite(append.clone()))
        else {
            panic!("upload state");
        };
        assert_eq!(replayed.status, terminal);
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT COUNT(*) FROM service_template_chunks", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        let mut changed = append.clone();
        changed.text = "{{transcript}} changed".into();
        changed.sha256 = sha(changed.text.as_bytes());
        assert!(matches!(
            store.service_desktop_templates(OWNER, &DesktopTemplatesRequest::AppendWrite(changed)),
            Err(DataError::IdempotencyConflict)
        ));
        let other = begin("other", body, "begin-other");
        call(&store, DesktopTemplatesRequest::BeginWrite(other.clone()));
        let mut reused = append;
        reused.upload_id = other.upload_id;
        assert!(matches!(
            store.service_desktop_templates(OWNER, &DesktopTemplatesRequest::AppendWrite(reused)),
            Err(DataError::IdempotencyConflict)
        ));
        let (hash,receipt):(String,String)=store.conn.lock().query_row("SELECT arguments_hash,outcome_json FROM service_idempotency WHERE principal=?1 AND tool='template_append' AND key='global-append-key'",[OWNER],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(hash.len(), 64);
        assert_eq!(receipt, "{}");
        assert!(!receipt.contains("PRIVATE_PAYLOAD"));
    }
}

#[test]
fn corrupt_negative_sql_counters_fail_without_wrapping_or_publishing() {
    let store = fixture();
    let body = "{{transcript}} exact";
    let upload = begin("negative-counter", body, "negative-counter");
    call(&store, DesktopTemplatesRequest::BeginWrite(upload.clone()));
    append(&store, &upload, body);
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_template_uploads SET received_bytes=-1 WHERE id=?1",
            [upload.upload_id.to_string()],
        )
        .unwrap();
    assert!(matches!(
        store
            .service_desktop_templates(OWNER, &DesktopTemplatesRequest::BeginWrite(upload.clone())),
        Err(DataError::Invalid("negative stored template count"))
    ));
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_template_uploads SET received_bytes=?2 WHERE id=?1",
            params![
                upload.upload_id.to_string(),
                sql_integer(body.len() as u64).unwrap()
            ],
        )
        .unwrap();
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_template_chunks SET byte_offset=-1 WHERE upload_id=?1",
            [upload.upload_id.to_string()],
        )
        .unwrap();
    let commit = DesktopTemplatesRequest::CommitWrite(TemplateUploadRequest {
        upload_id: upload.upload_id,
        idempotency_key: "negative-commit".into(),
    });
    assert!(matches!(
        store.service_desktop_templates(OWNER, &commit),
        Err(DataError::Invalid("negative stored template count"))
    ));
    assert_eq!(
        describe(&store, "negative-counter").state,
        TemplateDocumentState::Absent
    );
    assert_eq!(
        upload_on(&store.conn.lock(), upload.upload_id, OWNER)
            .unwrap()
            .public
            .status,
        TemplateUploadStatus::Open
    );
}
