-- v12: narrow desktop preferences control state; no legacy settings rewrite.
CREATE TABLE service_desktop_state(
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  revision TEXT NOT NULL
);
INSERT INTO service_desktop_state VALUES(1,lower(hex(randomblob(16))));
