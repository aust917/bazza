-- v14: durable private GUI organization revisions. No legacy meeting indexes.
CREATE TABLE service_folder_state(
  folder_id TEXT PRIMARY KEY REFERENCES folders(id) ON DELETE CASCADE,
  revision TEXT NOT NULL
);
INSERT INTO service_folder_state SELECT id,lower(hex(randomblob(16))) FROM folders;
CREATE TABLE service_tag_state(
  tag_id TEXT PRIMARY KEY REFERENCES tags(id) ON DELETE CASCADE,
  revision TEXT NOT NULL
);
INSERT INTO service_tag_state SELECT id,lower(hex(randomblob(16))) FROM tags;
