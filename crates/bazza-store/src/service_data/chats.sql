-- v17: private read projections. No ordinary v9 startup or writer routing changes.
-- These revisions survive changefeed pruning. Bodies remain solely in legacy tables.
CREATE TABLE service_chat_catalog(singleton INTEGER PRIMARY KEY CHECK(singleton=1),revision TEXT NOT NULL);
INSERT INTO service_chat_catalog VALUES(1,lower(hex(randomblob(16))));
CREATE TABLE service_chat_state(
 chat_id TEXT PRIMARY KEY,generation TEXT NOT NULL,metadata_revision TEXT NOT NULL,
 messages_revision TEXT NOT NULL,erasure_revision TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('present','deleted')),
 title_bytes INTEGER NOT NULL,scope_kind_bytes INTEGER NOT NULL,scope_id_bytes INTEGER
);
INSERT INTO service_chat_state
 SELECT id,lower(hex(randomblob(16))),lower(hex(randomblob(16))),lower(hex(randomblob(16))),lower(hex(randomblob(16))),
 'present',length(CAST(title AS BLOB)),length(CAST(scope_kind AS BLOB)),length(CAST(scope_id AS BLOB)) FROM chats;
CREATE TABLE service_chat_message_state(
 message_id TEXT PRIMARY KEY,chat_id TEXT NOT NULL,generation TEXT NOT NULL,revision TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('present','deleted')),
 role_bytes INTEGER NOT NULL,content_bytes INTEGER NOT NULL,citations_bytes INTEGER NOT NULL
);
CREATE INDEX service_chat_message_parent ON service_chat_message_state(chat_id);
INSERT INTO service_chat_message_state
 SELECT id,chat_id,lower(hex(randomblob(16))),lower(hex(randomblob(16))),'present',
 length(CAST(role AS BLOB)),length(CAST(content AS BLOB)),length(CAST(citations_json AS BLOB)) FROM chat_messages;

CREATE TRIGGER service_chats_insert AFTER INSERT ON chats BEGIN
 INSERT INTO service_chat_state VALUES(new.id,lower(hex(randomblob(16))),lower(hex(randomblob(16))),lower(hex(randomblob(16))),lower(hex(randomblob(16))),
 'present',length(CAST(new.title AS BLOB)),length(CAST(new.scope_kind AS BLOB)),length(CAST(new.scope_id AS BLOB)))
 ON CONFLICT(chat_id) DO UPDATE SET generation=excluded.generation,metadata_revision=excluded.metadata_revision,
 messages_revision=excluded.messages_revision,erasure_revision=excluded.erasure_revision,state='present',
 title_bytes=excluded.title_bytes,scope_kind_bytes=excluded.scope_kind_bytes,scope_id_bytes=excluded.scope_id_bytes;
 UPDATE service_chat_catalog SET revision=lower(hex(randomblob(16))) WHERE singleton=1;
END;
CREATE TRIGGER service_chats_update AFTER UPDATE ON chats BEGIN
 UPDATE service_chat_state SET state='deleted',metadata_revision=lower(hex(randomblob(16))),
 messages_revision=lower(hex(randomblob(16))),erasure_revision=lower(hex(randomblob(16))) WHERE chat_id=old.id AND old.id IS NOT new.id;
 INSERT INTO service_chat_state VALUES(new.id,lower(hex(randomblob(16))),lower(hex(randomblob(16))),lower(hex(randomblob(16))),lower(hex(randomblob(16))),
 'present',length(CAST(new.title AS BLOB)),length(CAST(new.scope_kind AS BLOB)),length(CAST(new.scope_id AS BLOB)))
 ON CONFLICT(chat_id) DO UPDATE SET
 generation=CASE WHEN old.id IS NOT new.id OR service_chat_state.state='deleted' THEN excluded.generation ELSE service_chat_state.generation END,
 metadata_revision=excluded.metadata_revision,state='present',title_bytes=excluded.title_bytes,
 scope_kind_bytes=excluded.scope_kind_bytes,scope_id_bytes=excluded.scope_id_bytes,
 erasure_revision=CASE WHEN old.scope_kind IS NOT new.scope_kind OR old.scope_id IS NOT new.scope_id THEN excluded.erasure_revision ELSE service_chat_state.erasure_revision END;
 UPDATE service_chat_catalog SET revision=lower(hex(randomblob(16))) WHERE singleton=1;
END;
CREATE TRIGGER service_chats_delete AFTER DELETE ON chats BEGIN
 UPDATE service_chat_state SET state='deleted',metadata_revision=lower(hex(randomblob(16))),
 messages_revision=lower(hex(randomblob(16))),erasure_revision=lower(hex(randomblob(16))) WHERE chat_id=old.id;
 UPDATE service_chat_catalog SET revision=lower(hex(randomblob(16))) WHERE singleton=1;
END;
CREATE TRIGGER service_chat_messages_insert AFTER INSERT ON chat_messages BEGIN
 INSERT INTO service_chat_message_state VALUES(new.id,new.chat_id,lower(hex(randomblob(16))),lower(hex(randomblob(16))),
 'present',length(CAST(new.role AS BLOB)),length(CAST(new.content AS BLOB)),length(CAST(new.citations_json AS BLOB)))
 ON CONFLICT(message_id) DO UPDATE SET chat_id=excluded.chat_id,generation=excluded.generation,revision=excluded.revision,state='present',
 role_bytes=excluded.role_bytes,content_bytes=excluded.content_bytes,citations_bytes=excluded.citations_bytes;
 UPDATE service_chat_state SET messages_revision=lower(hex(randomblob(16))) WHERE chat_id=new.chat_id;
 UPDATE service_chat_catalog SET revision=lower(hex(randomblob(16))) WHERE singleton=1;
END;
CREATE TRIGGER service_chat_messages_update AFTER UPDATE ON chat_messages BEGIN
 UPDATE service_chat_message_state SET state='deleted',revision=lower(hex(randomblob(16))) WHERE message_id=old.id AND old.id IS NOT new.id;
 INSERT INTO service_chat_message_state VALUES(new.id,new.chat_id,lower(hex(randomblob(16))),lower(hex(randomblob(16))),
 'present',length(CAST(new.role AS BLOB)),length(CAST(new.content AS BLOB)),length(CAST(new.citations_json AS BLOB)))
 ON CONFLICT(message_id) DO UPDATE SET chat_id=excluded.chat_id,
 generation=CASE WHEN old.id IS NOT new.id OR old.chat_id IS NOT new.chat_id OR service_chat_message_state.state='deleted' THEN excluded.generation ELSE service_chat_message_state.generation END,
 revision=excluded.revision,state='present',role_bytes=excluded.role_bytes,content_bytes=excluded.content_bytes,citations_bytes=excluded.citations_bytes;
 UPDATE service_chat_state SET messages_revision=lower(hex(randomblob(16))) WHERE chat_id IN (old.chat_id,new.chat_id);
 UPDATE service_chat_catalog SET revision=lower(hex(randomblob(16))) WHERE singleton=1;
END;
CREATE TRIGGER service_chat_messages_delete AFTER DELETE ON chat_messages BEGIN
 UPDATE service_chat_message_state SET state='deleted',revision=lower(hex(randomblob(16))) WHERE message_id=old.id;
 UPDATE service_chat_state SET messages_revision=lower(hex(randomblob(16))) WHERE chat_id=old.chat_id;
 UPDATE service_chat_catalog SET revision=lower(hex(randomblob(16))) WHERE singleton=1;
END;
